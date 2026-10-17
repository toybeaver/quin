//! Frames: how the window is split into views.
//!
//! Each tab owns a tree of splits whose leaves are views. A view is a window
//! onto a buffer with its own selection and scroll position.

use std::collections::HashMap;

use crate::buffer::BufferId;
use crate::render::Rect;
use crate::selection::Selection;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ViewId(pub usize);

/// Jump list entry: where the cursor was before a big jump.
#[derive(Debug, Clone)]
pub struct Jump {
    pub buffer: BufferId,
    pub selection: Selection,
}

#[derive(Debug, Clone)]
pub struct View {
    pub id: ViewId,
    pub buffer: BufferId,
    pub selection: Selection,
    /// First visible line.
    pub scroll_line: usize,
    /// First visible column (in cells).
    pub scroll_col: usize,
    /// Visible size in cells, updated when the view is drawn.
    pub rows: usize,
    pub cols: usize,
    pub jumps: Vec<Jump>,
    pub jump_index: usize,
    /// Selections remembered per buffer, so switching back to a buffer in this
    /// view restores where the cursor was.
    pub saved_selections: HashMap<BufferId, Selection>,
    /// A scroll in progress: the view slides from `from` to `scroll_line`.
    pub anim: Option<ScrollAnim>,
    /// Where the primary cursor was last drawn (pixels inside the frame) and
    /// the slide towards its new place, if one is running.
    pub cursor_px: Option<(f32, f32)>,
    pub cursor_anim: Option<CursorAnim>,
    /// A terminal frame the user scrolled up into the scrollback: stop
    /// following the live screen until a key is sent.
    pub scrolled_back: bool,
    /// The wheel moved the view: at the next draw the cursor moves into the
    /// view, instead of the view moving back to the cursor.
    pub cursor_follows: bool,
    /// A preview frame (the git status buffer's split): no edits here.
    pub readonly: bool,
}

#[derive(Debug, Clone)]
pub struct CursorAnim {
    pub from: (f32, f32),
    pub started: std::time::Instant,
}

#[derive(Debug, Clone)]
pub struct ScrollAnim {
    pub from: f32,
    pub started: std::time::Instant,
}

impl View {
    pub fn new(id: ViewId, buffer: BufferId) -> View {
        View {
            id,
            buffer,
            selection: Selection::point(0),
            scroll_line: 0,
            scroll_col: 0,
            rows: 24,
            cols: 80,
            jumps: Vec::new(),
            jump_index: 0,
            cursor_follows: false,
            saved_selections: HashMap::new(),
            anim: None,
            cursor_px: None,
            cursor_anim: None,
            scrolled_back: false,
            readonly: false,
        }
    }

    /// Where to draw the primary cursor now, given where it belongs: starts a
    /// slide from the last drawn place when that changed (`ms` = 0 disables).
    pub fn cursor_position(&mut self, target: (f32, f32), ms: u64) -> (f32, f32) {
        let shown = self.shown_cursor(ms);
        if self.cursor_px != Some(target) {
            if ms > 0
                && let Some(previous) = shown
            {
                self.cursor_anim = Some(CursorAnim { from: previous, started: std::time::Instant::now() });
            }
            self.cursor_px = Some(target);
        }
        self.shown_cursor(ms).unwrap_or(target)
    }

    fn shown_cursor(&mut self, ms: u64) -> Option<(f32, f32)> {
        let target = self.cursor_px?;
        let Some(anim) = &self.cursor_anim else { return Some(target) };
        let t = anim.started.elapsed().as_secs_f32() / (ms.max(1) as f32 / 1000.0);
        if t >= 1.0 {
            self.cursor_anim = None;
            return Some(target);
        }
        let eased = 1.0 - (1.0 - t).powi(3);
        Some((anim.from.0 + (target.0 - anim.from.0) * eased, anim.from.1 + (target.1 - anim.from.1) * eased))
    }

    /// Change the first visible line, sliding there over `ms` milliseconds
    /// (jumps of one line, and `ms` = 0, move at once).
    pub fn scroll_to(&mut self, line: usize, ms: u64) {
        if line == self.scroll_line {
            return;
        }
        if ms > 0 && line.abs_diff(self.scroll_line) > 1 {
            let from = self.displayed_scroll(ms);
            self.anim = Some(ScrollAnim { from, started: std::time::Instant::now() });
        } else {
            self.anim = None;
        }
        self.scroll_line = line;
    }

    /// Where the view appears to be scrolled right now, as a fractional line.
    pub fn displayed_scroll(&mut self, ms: u64) -> f32 {
        let Some(anim) = &self.anim else { return self.scroll_line as f32 };
        let t = anim.started.elapsed().as_secs_f32() / (ms.max(1) as f32 / 1000.0);
        if t >= 1.0 {
            self.anim = None;
            return self.scroll_line as f32;
        }
        let eased = 1.0 - (1.0 - t).powi(3);
        anim.from + (self.scroll_line as f32 - anim.from) * eased
    }

    pub fn cursor(&self) -> usize {
        self.selection.primary().head
    }

    /// Show `buffer` in this view, remembering the selection of the old one.
    pub fn switch_buffer(&mut self, buffer: BufferId) {
        if buffer == self.buffer {
            return;
        }
        self.saved_selections.insert(self.buffer, self.selection.clone());
        self.buffer = buffer;
        self.selection = self.saved_selections.get(&buffer).cloned().unwrap_or_else(|| Selection::point(0));
        self.scroll_line = 0;
        self.scroll_col = 0;
        // No slide across buffers.
        self.cursor_px = None;
        self.cursor_anim = None;
    }

    /// Push the current position on the jump list (`C-s`, before `G`, searches...).
    pub fn save_jump(&mut self) {
        self.jumps.truncate(self.jump_index);
        let jump = Jump { buffer: self.buffer, selection: self.selection.clone() };
        if self.jumps.last().is_some_and(|j| j.buffer == jump.buffer && j.selection == jump.selection) {
            return;
        }
        self.jumps.push(jump);
        if self.jumps.len() > 100 {
            self.jumps.remove(0);
        }
        self.jump_index = self.jumps.len();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    /// Children side by side (a "vertical split" in vim terms).
    Horizontal,
    /// Children stacked (a "horizontal split" in vim terms).
    Vertical,
}

#[derive(Debug, Clone)]
pub enum Node {
    Leaf(ViewId),
    Split {
        dir: Dir,
        children: Vec<Node>,
        /// Relative sizes of the children.
        weights: Vec<f32>,
        /// Sizes to go back to (`space w 0`, closing a frame): equal shares,
        /// except for a sidebar made with `split_root`.
        defaults: Vec<f32>,
    },
}

/// Width of the line drawn between two frames.
pub const SEPARATOR: i32 = 2;

impl Node {
    pub fn leaves(&self) -> Vec<ViewId> {
        let mut out = Vec::new();
        self.collect_leaves(&mut out);
        out
    }

    fn collect_leaves(&self, out: &mut Vec<ViewId>) {
        match self {
            Node::Leaf(id) => out.push(*id),
            Node::Split { children, .. } => children.iter().for_each(|c| c.collect_leaves(out)),
        }
    }

    pub fn contains(&self, view: ViewId) -> bool {
        match self {
            Node::Leaf(id) => *id == view,
            Node::Split { children, .. } => children.iter().any(|c| c.contains(view)),
        }
    }

    /// Compute the rectangle of every leaf inside `rect`.
    pub fn layout(&self, rect: Rect, out: &mut Vec<(ViewId, Rect)>) {
        match self {
            Node::Leaf(id) => out.push((*id, rect)),
            Node::Split { dir, children, weights, .. } => {
                let total: f32 = weights.iter().sum();
                let separators = SEPARATOR * (children.len() as i32 - 1);
                let available = match dir {
                    Dir::Horizontal => rect.w - separators,
                    Dir::Vertical => rect.h - separators,
                } as f32;
                let mut offset = 0;
                let mut used = 0;
                for (i, (child, weight)) in children.iter().zip(weights).enumerate() {
                    let last = i + 1 == children.len();
                    let size = if last { (available as i32 - used).max(0) } else { (available * weight / total).round() as i32 };
                    let child_rect = match dir {
                        Dir::Horizontal => Rect::new(rect.x + offset, rect.y, size, rect.h),
                        Dir::Vertical => Rect::new(rect.x, rect.y + offset, rect.w, size),
                    };
                    child.layout(child_rect, out);
                    offset += size + SEPARATOR;
                    used += size;
                }
            }
        }
    }

    /// Split the leaf `target`, putting `new` after it (right or below).
    pub fn split(&mut self, target: ViewId, dir: Dir, new: ViewId) -> bool {
        match self {
            Node::Leaf(id) if *id == target => {
                *self = Node::Split {
                    dir,
                    children: vec![Node::Leaf(target), Node::Leaf(new)],
                    weights: vec![1.0, 1.0],
                    defaults: vec![1.0, 1.0],
                };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { dir: own_dir, children, weights, defaults } => {
                // Same direction: insert as a sibling instead of nesting.
                if *own_dir == dir
                    && let Some(i) = children.iter().position(|c| matches!(c, Node::Leaf(id) if *id == target))
                {
                    weights.insert(i + 1, 1.0);
                    defaults.insert(i + 1, 1.0);
                    children.insert(i + 1, Node::Leaf(new));
                    return true;
                }
                children.iter_mut().any(|c| c.split(target, dir, new))
            }
        }
    }

    /// Remove a leaf. Returns false if it was not found (or is the only one).
    pub fn remove(&mut self, target: ViewId) -> bool {
        let Node::Split { children, weights, defaults, .. } = self else { return false };
        if let Some(i) = children.iter().position(|c| matches!(c, Node::Leaf(id) if *id == target)) {
            children.remove(i);
            weights.remove(i);
            defaults.remove(i);
            if children.len() == 1 {
                *self = children.remove(0);
            }
            return true;
        }
        for child in children.iter_mut() {
            if child.remove(target) {
                return true;
            }
        }
        false
    }

    /// Grow (`delta` > 0) or shrink the frame `view` along `dir`, taking the
    /// space from the frame right next to it in the closest split of that
    /// direction. `delta` is a fraction of that split.
    /// Returns `None` when `view` is not in this subtree, `Some(false)` when it
    /// is but no split of that direction surrounds it.
    pub fn resize(&mut self, view: ViewId, dir: Dir, delta: f32) -> Option<bool> {
        match self {
            Node::Leaf(id) => (*id == view).then_some(false),
            Node::Split { dir: own_dir, children, weights, .. } => {
                let index = children.iter().position(|c| c.contains(view))?;
                if children[index].resize(view, dir, delta) == Some(true) {
                    return Some(true); // a split closer to the frame took care of it
                }
                if *own_dir != dir || children.len() < 2 {
                    return Some(false);
                }
                let neighbour = if index + 1 < children.len() { index + 1 } else { index - 1 };
                let total: f32 = weights.iter().sum();
                let step = (delta * total).clamp(-(weights[index] - MIN_FRACTION * total), weights[neighbour] - MIN_FRACTION * total);
                weights[index] += step;
                weights[neighbour] -= step;
                // When the neighbour is itself a row of frames, only the one
                // touching us gives up (or gains) the space.
                let old_weight = weights[neighbour] + step;
                children[neighbour].shift_edge(dir, -step, old_weight, neighbour > index);
                Some(true)
            }
        }
    }

    /// Apply a size change of `amount` (in the parent's weight units; this
    /// node weighed `old_weight`) to the child at the start or the end only.
    fn shift_edge(&mut self, dir: Dir, amount: f32, old_weight: f32, first: bool) {
        let Node::Split { dir: own_dir, children, weights, .. } = self else { return };
        if *own_dir != dir || old_weight <= 0.0 {
            return;
        }
        let total: f32 = weights.iter().sum();
        for weight in weights.iter_mut() {
            *weight *= old_weight / total;
        }
        let edge = if first { 0 } else { children.len() - 1 };
        let before = weights[edge];
        weights[edge] = (before + amount).max(MIN_FRACTION * old_weight);
        children[edge].shift_edge(dir, weights[edge] - before, before, first);
    }

    /// Put every split back to the sizes it was created with.
    pub fn reset_weights(&mut self) {
        if let Node::Split { children, weights, defaults, .. } = self {
            weights.clone_from(defaults);
            children.iter_mut().for_each(Node::reset_weights);
        }
    }

    /// Wrap the whole tree in a split with a new leaf taking `fraction` of the
    /// space on the right (or bottom).
    pub fn split_root(&mut self, dir: Dir, new: ViewId, fraction: f32) {
        let old = std::mem::replace(self, Node::Leaf(new));
        *self = Node::Split {
            dir,
            children: vec![old, Node::Leaf(new)],
            weights: vec![1.0 - fraction, fraction],
            defaults: vec![1.0 - fraction, fraction],
        };
    }

    pub fn replace(&mut self, old: ViewId, new: ViewId) {
        match self {
            Node::Leaf(id) if *id == old => *id = new,
            Node::Leaf(_) => {}
            Node::Split { children, .. } => children.iter_mut().for_each(|c| c.replace(old, new)),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Tab {
    pub root: Node,
    pub focus: ViewId,
}

impl Tab {
    pub fn new(view: ViewId) -> Tab {
        Tab { root: Node::Leaf(view), focus: view }
    }
}

/// No frame shrinks below this share of its split.
const MIN_FRACTION: f32 = 0.1;

/// Which frame lies in a direction from `from`, judged by geometry.
pub fn neighbor(rects: &[(ViewId, Rect)], from: ViewId, dir: Direction) -> Option<ViewId> {
    let (_, cur) = rects.iter().find(|(id, _)| *id == from)?;
    let mut best: Option<(ViewId, i32, i32)> = None;
    for (id, r) in rects {
        if id == &from {
            continue;
        }
        let (adjacent, distance, overlap) = match dir {
            Direction::Left => (r.right() <= cur.x, cur.x - r.right(), overlap(r.y, r.bottom(), cur.y, cur.bottom())),
            Direction::Right => (r.x >= cur.right(), r.x - cur.right(), overlap(r.y, r.bottom(), cur.y, cur.bottom())),
            Direction::Up => (r.bottom() <= cur.y, cur.y - r.bottom(), overlap(r.x, r.right(), cur.x, cur.right())),
            Direction::Down => (r.y >= cur.bottom(), r.y - cur.bottom(), overlap(r.x, r.right(), cur.x, cur.right())),
        };
        if !adjacent || overlap <= 0 {
            continue;
        }
        if best.is_none_or(|(_, d, o)| distance < d || (distance == d && overlap > o)) {
            best = Some((*id, distance, overlap));
        }
    }
    best.map(|(id, _, _)| id)
}

fn overlap(a0: i32, a1: i32, b0: i32, b1: i32) -> i32 {
    a1.min(b1) - a0.max(b0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_and_remove() {
        let mut root = Node::Leaf(ViewId(1));
        assert!(root.split(ViewId(1), Dir::Horizontal, ViewId(2)));
        assert!(root.split(ViewId(2), Dir::Horizontal, ViewId(3)));
        assert_eq!(root.leaves(), vec![ViewId(1), ViewId(2), ViewId(3)]);
        if let Node::Split { children, .. } = &root {
            assert_eq!(children.len(), 3, "same-direction splits stay flat");
        }
        assert!(root.remove(ViewId(2)));
        assert!(root.remove(ViewId(3)));
        assert!(matches!(root, Node::Leaf(ViewId(1))));
    }

    #[test]
    fn layout_and_neighbors() {
        let mut root = Node::Leaf(ViewId(1));
        root.split(ViewId(1), Dir::Horizontal, ViewId(2));
        root.split(ViewId(2), Dir::Vertical, ViewId(3));
        let mut rects = Vec::new();
        root.layout(Rect::new(0, 0, 201, 101), &mut rects);
        assert_eq!(rects.len(), 3);
        assert_eq!(neighbor(&rects, ViewId(1), Direction::Right), Some(ViewId(2)));
        assert_eq!(neighbor(&rects, ViewId(2), Direction::Down), Some(ViewId(3)));
        assert_eq!(neighbor(&rects, ViewId(3), Direction::Left), Some(ViewId(1)));
        assert_eq!(neighbor(&rects, ViewId(1), Direction::Left), None);
    }
}
