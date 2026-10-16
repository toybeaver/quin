//! Git signs: which lines of an open file were added, modified or deleted
//! compared to HEAD, drawn as coloured marks on the left edge of the frame.
//!
//! The file's version at HEAD (its "base") is read with `git show` on a
//! background thread, again whenever the repository status changes. The
//! buffer's text, unsaved edits included, is compared with that base on
//! another background thread after every edit. Nothing here waits on git or
//! on the diff: until an answer comes the last marks stay on screen.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use ropey::Rope;

use crate::buffer::{Buffer, BufferId};
use crate::terminal::WakeFn;

/// A diff that needs more edits than this is not worked out line by line:
/// the whole changed middle of the file is marked as modified instead.
const MAX_EDITS: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sign {
    Added,
    Modified,
}

/// The signs of one file, by line of the buffer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Marks {
    pub lines: HashMap<usize, Sign>,
    /// Lines in front of which lines were deleted.
    pub deleted_before: HashSet<usize>,
}

enum Base {
    /// `git show` is running.
    Loading,
    /// Not in a repository, or ignored: no signs.
    None,
    /// The lines of the file at HEAD (empty for a file HEAD does not have).
    Ready(Arc<Vec<String>>),
}

struct Entry {
    path: PathBuf,
    base: Base,
    /// The `generation` the base was read in.
    base_generation: u64,
    marks: Marks,
    /// The buffer version `marks` describe.
    marks_version: Option<u64>,
    diffing: bool,
}

enum Message {
    Base { buffer: BufferId, generation: u64, base: Option<Vec<String>> },
    Marks { buffer: BufferId, version: u64, marks: Marks },
}

pub struct Signs {
    buffers: HashMap<BufferId, Entry>,
    /// Bumped when HEAD may have moved: every base is read again.
    generation: u64,
    sender: Sender<Message>,
    receiver: Receiver<Message>,
}

impl Default for Signs {
    fn default() -> Signs {
        let (sender, receiver) = channel();
        Signs { buffers: HashMap::new(), generation: 0, sender, receiver }
    }
}

impl Signs {
    /// Read the base of every file again (a commit, pull or checkout may
    /// have moved HEAD). The current marks stay until the new ones are ready.
    pub fn reload(&mut self) {
        self.generation += 1;
    }

    /// The signs to draw for a buffer.
    pub fn marks(&self, buffer: BufferId) -> Option<&Marks> {
        self.buffers.get(&buffer).map(|e| &e.marks)
    }

    /// Take in finished work. True when some marks changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(message) = self.receiver.try_recv() {
            match message {
                Message::Base { buffer, generation, base } => {
                    let Some(entry) = self.buffers.get_mut(&buffer) else { continue };
                    entry.base_generation = generation;
                    entry.marks_version = None;
                    entry.base = match base {
                        Some(lines) => Base::Ready(Arc::new(lines)),
                        None => {
                            changed |= entry.marks != Marks::default();
                            entry.marks = Marks::default();
                            Base::None
                        }
                    };
                }
                Message::Marks { buffer, version, marks } => {
                    let Some(entry) = self.buffers.get_mut(&buffer) else { continue };
                    entry.diffing = false;
                    entry.marks_version = Some(version);
                    changed |= entry.marks != marks;
                    entry.marks = marks;
                }
            }
        }
        changed
    }

    /// Start what `buffer` needs for its signs to be current: reading its
    /// base, or comparing its text with the base. Cheap when nothing is due,
    /// so drawing calls it for every visible file.
    pub fn update(&mut self, buffer: &Buffer, wake: Option<&WakeFn>) {
        if buffer.is_terminal() || buffer.tree || buffer.git || buffer.welcome.is_some() || buffer.commit.is_some() {
            return;
        }
        let Some(path) = &buffer.path else { return };
        let entry = self.buffers.entry(buffer.id).or_insert_with(|| Entry::new(path));
        if entry.path != *path {
            // Saved under another name: a different file in git.
            *entry = Entry::new(path);
        }
        let loading = matches!(entry.base, Base::Loading);
        if entry.base_generation != self.generation && !loading {
            entry.base = Base::Loading;
            let (sender, wake, id, generation, path) = (self.sender.clone(), wake.cloned(), buffer.id, self.generation, path.clone());
            std::thread::spawn(move || {
                let base = read_base(&path);
                let _ = sender.send(Message::Base { buffer: id, generation, base });
                if let Some(wake) = wake {
                    wake();
                }
            });
            return;
        }
        if let Base::Ready(base) = &entry.base
            && !entry.diffing
            && entry.marks_version != Some(buffer.version)
        {
            entry.diffing = true;
            let (sender, wake, id, version) = (self.sender.clone(), wake.cloned(), buffer.id, buffer.version);
            let (base, text) = (base.clone(), buffer.text.clone());
            std::thread::spawn(move || {
                let marks = marks_for(&base, &text);
                let _ = sender.send(Message::Marks { buffer: id, version, marks });
                if let Some(wake) = wake {
                    wake();
                }
            });
        }
    }

    /// True while a base is being read or a diff computed.
    pub fn busy(&self) -> bool {
        self.buffers.values().any(|e| e.diffing || matches!(e.base, Base::Loading))
    }

    /// Drop what is kept for buffers that were closed.
    pub fn forget_closed(&mut self, open: &HashMap<BufferId, Buffer>) {
        self.buffers.retain(|id, _| open.contains_key(id));
    }
}

impl Entry {
    /// Nothing known yet: the base is read on the next `update`.
    fn new(path: &Path) -> Entry {
        Entry {
            path: path.to_path_buf(),
            base: Base::None,
            base_generation: u64::MAX,
            marks: Marks::default(),
            marks_version: None,
            diffing: false,
        }
    }
}

/// The lines of `path` at HEAD: empty when HEAD does not have the file (it is
/// all new), `None` outside a repository or for an ignored file.
fn read_base(path: &Path) -> Option<Vec<String>> {
    let (dir, name) = (path.parent()?, path.file_name()?.to_string_lossy());
    let (_, inside) = crate::git::run_git(dir, &["rev-parse", "--is-inside-work-tree"], None);
    if inside.trim() != "true" {
        return None;
    }
    let (ignored, _) = crate::git::run_git(dir, &["check-ignore", "-q", "--", &name], None);
    if ignored {
        return None;
    }
    // `HEAD:./name` is relative to the directory git runs in.
    let (found, text) = crate::git::run_git(dir, &["show", &format!("HEAD:./{name}")], None);
    if !found {
        return Some(Vec::new());
    }
    Some(text.lines().map(|l| l.trim_end_matches('\r').to_string()).collect())
}

/// Compare the buffer's text with its base and mark its lines.
pub fn marks_for(base: &[String], text: &Rope) -> Marks {
    let text = text.to_string();
    let new: Vec<&str> = text.lines().collect();
    let old: Vec<&str> = base.iter().map(|l| l.as_str()).collect();
    let mut marks = Marks::default();
    for block in changed_blocks(&old, &new) {
        let (removed, added) = (block.old.len(), block.new.len());
        let modified = removed.min(added);
        for line in block.new.clone() {
            let sign = if line < block.new.start + modified { Sign::Modified } else { Sign::Added };
            marks.lines.insert(line, sign);
        }
        if removed > added {
            // Deleted at the end of the file: mark the last line.
            let at = block.new.end.min(new.len().saturating_sub(1));
            marks.deleted_before.insert(at);
        }
    }
    marks
}

/// A run of lines that differ: `old` lines were replaced by `new` lines
/// (either may be empty).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub old: std::ops::Range<usize>,
    pub new: std::ops::Range<usize>,
}

/// The blocks where `new` differs from `old`. Lines the two share at the
/// start and at the end are skipped first, then the middle is compared with
/// Myers' algorithm (the shortest list of line insertions and deletions).
pub fn changed_blocks(old: &[&str], new: &[&str]) -> Vec<Block> {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..].iter().rev().zip(new[prefix..].iter().rev()).take_while(|(a, b)| a == b).count();
    let (old_mid, new_mid) = (&old[prefix..old.len() - suffix], &new[prefix..new.len() - suffix]);
    if old_mid.is_empty() && new_mid.is_empty() {
        return Vec::new();
    }
    let Some(pairs) = matching_lines(old_mid, new_mid) else {
        // Too different to be worth the work: one block for the whole middle.
        return vec![Block { old: prefix..old.len() - suffix, new: prefix..new.len() - suffix }];
    };
    // The gaps between matching lines are the changed blocks.
    let mut blocks = Vec::new();
    let (mut i, mut j) = (0, 0);
    for (a, b) in pairs.into_iter().chain([(old_mid.len(), new_mid.len())]) {
        if a > i || b > j {
            blocks.push(Block { old: prefix + i..prefix + a, new: prefix + j..prefix + b });
        }
        (i, j) = (a + 1, b + 1);
    }
    blocks
}

/// Pairs of equal lines (index in `old`, index in `new`), in order, that
/// leave the fewest lines to insert or delete. `None` when more than
/// `MAX_EDITS` edits are needed.
///
/// Myers' algorithm: for d = 0, 1, 2... it records, on every diagonal k = x - y,
/// the furthest point (x in `old`, y in `new`) reachable with d edits,
/// following runs of equal lines for free. The first d to reach the end is
/// the shortest edit script; walking the recorded steps back gives the pairs.
fn matching_lines(old: &[&str], new: &[&str]) -> Option<Vec<(usize, usize)>> {
    let (n, m) = (old.len() as isize, new.len() as isize);
    let max = (old.len() + new.len()).min(MAX_EDITS) as isize;
    let offset = max + 1;
    // furthest[offset + k]: furthest x on diagonal k so far.
    let mut furthest = vec![0isize; 2 * max as usize + 3];
    // The `furthest` values of diagonals -d..=d before step d, for walking back.
    let mut steps: Vec<Vec<isize>> = Vec::new();
    for d in 0..=max {
        steps.push(furthest[(offset - d) as usize..=(offset + d) as usize].to_vec());
        for k in (-d..=d).step_by(2) {
            let down = k == -d || (k != d && furthest[(offset + k - 1) as usize] < furthest[(offset + k + 1) as usize]);
            let mut x = if down { furthest[(offset + k + 1) as usize] } else { furthest[(offset + k - 1) as usize] + 1 };
            let mut y = x - k;
            while x < n && y < m && old[x as usize] == new[y as usize] {
                x += 1;
                y += 1;
            }
            furthest[(offset + k) as usize] = x;
            if x >= n && y >= m {
                return Some(walk_back(&steps, n, m));
            }
        }
    }
    None
}

/// Follow the recorded steps from the end back to the start, collecting the
/// runs of equal lines ("snakes") that each step ended with.
fn walk_back(steps: &[Vec<isize>], n: isize, m: isize) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    let (mut x, mut y) = (n, m);
    for (d, before) in steps.iter().enumerate().rev() {
        let d = d as isize;
        if d == 0 {
            // The first run of equal lines, from the very start.
            while x > 0 && y > 0 {
                x -= 1;
                y -= 1;
                pairs.push((x as usize, y as usize));
            }
            break;
        }
        // `before` holds diagonals -d..=d as they were before step d.
        let at = |k: isize| before[(k + d) as usize];
        let k = x - y;
        let previous_k = if k == -d || (k != d && at(k - 1) < at(k + 1)) { k + 1 } else { k - 1 };
        let previous_x = at(previous_k);
        let previous_y = previous_x - previous_k;
        while x > previous_x && y > previous_y {
            x -= 1;
            y -= 1;
            pairs.push((x as usize, y as usize));
        }
        (x, y) = (previous_x, previous_y);
    }
    pairs.reverse();
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocks(old: &str, new: &str) -> Vec<(std::ops::Range<usize>, std::ops::Range<usize>)> {
        let (old, new): (Vec<&str>, Vec<&str>) = (old.lines().collect(), new.lines().collect());
        changed_blocks(&old, &new).into_iter().map(|b| (b.old, b.new)).collect()
    }

    #[test]
    fn finds_changed_blocks() {
        assert_eq!(blocks("a\nb\nc\n", "a\nb\nc\n"), vec![]);
        assert_eq!(blocks("a\nb\nc\n", "a\nX\nc\n"), vec![(1..2, 1..2)]);
        assert_eq!(blocks("a\nb\nc\n", "a\nb\nnew\nc\n"), vec![(2..2, 2..3)]);
        assert_eq!(blocks("a\nb\nc\n", "a\nc\n"), vec![(1..2, 1..1)]);
        // Two separate changes with equal lines between them.
        assert_eq!(blocks("a\nb\nc\nd\ne\n", "a\nB\nc\nd\nE\nf\n"), vec![(1..2, 1..2), (4..5, 4..6)]);
        // A moved line is a deletion and an insertion.
        assert_eq!(blocks("x\na\nb\n", "a\nb\nx\n"), vec![(0..1, 0..0), (3..3, 2..3)]);
        assert_eq!(blocks("", "a\nb\n"), vec![(0..0, 0..2)]);
    }

    #[test]
    fn finds_the_shortest_diff_inside_a_changed_middle() {
        // Prefix/suffix trimming leaves "b c d" vs "c d e": c and d still match.
        assert_eq!(blocks("a\nb\nc\nd\nz\n", "a\nc\nd\ne\nz\n"), vec![(1..2, 1..1), (4..4, 3..4)]);
        let old: Vec<String> = (0..300).map(|i| format!("line {i}")).collect();
        let mut new = old.clone();
        new.remove(10);
        new.insert(150, "inserted".into());
        new[200] = "changed".into();
        let (old, new): (Vec<&str>, Vec<&str>) = (old.iter().map(|s| s.as_str()).collect(), new.iter().map(|s| s.as_str()).collect());
        let found = changed_blocks(&old, &new);
        assert_eq!(found.len(), 3, "{found:?}");
    }

    #[test]
    fn random_diffs_are_correct_and_shortest() {
        // A small fixed pseudo-random generator: the test is the same every run.
        let mut seed = 12345u64;
        let mut next = |limit: u64| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) % limit
        };
        for _ in 0..500 {
            let old: Vec<String> = (0..next(12)).map(|_| ["a", "b", "c", "d"][next(4) as usize].to_string()).collect();
            let new: Vec<String> = (0..next(12)).map(|_| ["a", "b", "c", "d"][next(4) as usize].to_string()).collect();
            let (old, new): (Vec<&str>, Vec<&str>) = (old.iter().map(|s| s.as_str()).collect(), new.iter().map(|s| s.as_str()).collect());
            let found = changed_blocks(&old, &new);
            // Applying the blocks to `old` gives `new`.
            let mut rebuilt: Vec<&str> = Vec::new();
            let mut at = 0;
            for block in &found {
                rebuilt.extend(&old[at..block.old.start]);
                rebuilt.extend(&new[block.new.clone()]);
                at = block.old.end;
            }
            rebuilt.extend(&old[at..]);
            assert_eq!(rebuilt, new, "{old:?} -> {new:?}: {found:?}");
            // As few edits as the longest common subsequence allows.
            let mut lcs = vec![vec![0usize; new.len() + 1]; old.len() + 1];
            for i in (0..old.len()).rev() {
                for j in (0..new.len()).rev() {
                    lcs[i][j] = if old[i] == new[j] { lcs[i + 1][j + 1] + 1 } else { lcs[i + 1][j].max(lcs[i][j + 1]) };
                }
            }
            let edits: usize = found.iter().map(|b| b.old.len() + b.new.len()).sum();
            assert_eq!(edits, old.len() + new.len() - 2 * lcs[0][0], "{old:?} -> {new:?}: {found:?}");
        }
    }

    #[test]
    fn marks_added_modified_and_deleted_lines() {
        let base: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
        let marks = marks_for(&base, &Rope::from_str("a\nB\nnew\nc\n"));
        assert_eq!(marks.lines.get(&1), Some(&Sign::Modified));
        assert_eq!(marks.lines.get(&2), Some(&Sign::Added));
        assert_eq!(marks.lines.get(&0), None);
        assert_eq!(marks.deleted_before, HashSet::from([3]), "d was deleted at the end: on the last line");
        let marks = marks_for(&base, &Rope::from_str("a\nd\n"));
        assert!(marks.lines.is_empty());
        assert_eq!(marks.deleted_before, HashSet::from([1]), "b and c were deleted in front of d");
        let all_new = marks_for(&[], &Rope::from_str("x\ny\n"));
        assert_eq!(all_new.lines.len(), 2);
    }
}
