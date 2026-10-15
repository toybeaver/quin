//! Selections: every cursor is a one-character-wide selection, like in Helix/Kakoune.
//!
//! A `Range` is inclusive: it covers every character from `min(anchor, head)` to
//! `max(anchor, head)`. The `head` is where the block cursor is drawn and the side
//! that moves; the `anchor` is the fixed side. A `Selection` is a non-empty list of
//! ranges, one of them being the "primary" one (the one shown in the status bar).

use ropey::Rope;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub anchor: usize,
    pub head: usize,
    /// Remembered visual column for vertical movement (`j`/`k`), so the cursor
    /// comes back to the same column after passing over shorter lines.
    pub goal_col: Option<usize>,
}

impl Range {
    pub fn new(anchor: usize, head: usize) -> Range {
        Range { anchor, head, goal_col: None }
    }

    pub fn point(pos: usize) -> Range {
        Range::new(pos, pos)
    }

    /// First character index covered by the range.
    pub fn from(&self) -> usize {
        self.anchor.min(self.head)
    }

    /// One past the last character index covered by the range (unclamped).
    pub fn to(&self) -> usize {
        self.anchor.max(self.head) + 1
    }

    /// `[from, to)` clamped to the text length, suitable for slicing and deleting.
    pub fn span(&self, text: &Rope) -> (usize, usize) {
        let len = text.len_chars();
        (self.from().min(len), self.to().min(len))
    }

    pub fn is_forward(&self) -> bool {
        self.head >= self.anchor
    }

    /// Collapse to a single cursor placed at the head.
    pub fn collapsed(&self) -> Range {
        Range::point(self.head)
    }

    pub fn flipped(&self) -> Range {
        Range { anchor: self.head, head: self.anchor, goal_col: None }
    }

    /// Move the head; the anchor follows unless `extend` is set.
    pub fn with_head(&self, head: usize, extend: bool) -> Range {
        if extend { Range { anchor: self.anchor, head, goal_col: None } } else { Range::point(head) }
    }

    pub fn overlaps(&self, other: &Range) -> bool {
        self.from() < other.to() && other.from() < self.to()
    }

    /// Smallest range containing both.
    pub fn merge(&self, other: &Range) -> Range {
        let from = self.from().min(other.from());
        let last = (self.to().max(other.to())).saturating_sub(1);
        if self.is_forward() { Range::new(from, last) } else { Range::new(last, from) }
    }

    pub fn clamp(&self, len: usize) -> Range {
        Range { anchor: self.anchor.min(len), head: self.head.min(len), goal_col: self.goal_col }
    }

    /// The text covered by this range.
    pub fn slice(&self, text: &Rope) -> String {
        let (from, to) = self.span(text);
        text.slice(from..to).to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    ranges: Vec<Range>,
    primary: usize,
}

impl Selection {
    pub fn single(range: Range) -> Selection {
        Selection { ranges: vec![range], primary: 0 }
    }

    pub fn point(pos: usize) -> Selection {
        Selection::single(Range::point(pos))
    }

    /// Build from several ranges; they get sorted and overlapping ones merged.
    pub fn from_ranges(ranges: Vec<Range>, primary: usize) -> Selection {
        let mut sel = Selection { ranges, primary };
        if sel.ranges.is_empty() {
            sel.ranges.push(Range::point(0));
        }
        sel.normalize();
        sel
    }

    pub fn ranges(&self) -> &[Range] {
        &self.ranges
    }

    pub fn primary(&self) -> Range {
        self.ranges[self.primary]
    }

    pub fn primary_index(&self) -> usize {
        self.primary
    }

    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Range> {
        self.ranges.iter()
    }

    /// Apply `f` to every range and re-normalize.
    pub fn transform(&self, mut f: impl FnMut(Range) -> Range) -> Selection {
        let ranges = self.ranges.iter().map(|r| f(*r)).collect();
        Selection::from_ranges(ranges, self.primary)
    }

    /// Keep only the primary range.
    pub fn keep_primary(&self) -> Selection {
        Selection::single(self.primary())
    }

    pub fn set_primary(&mut self, index: usize) {
        self.primary = index.min(self.ranges.len() - 1);
    }

    pub fn clamp(&self, len: usize) -> Selection {
        self.transform(|r| r.clamp(len))
    }

    /// Sort ranges by position and merge the ones that overlap, keeping track of
    /// which range is the primary one.
    fn normalize(&mut self) {
        if self.ranges.len() == 1 {
            self.primary = 0;
            return;
        }
        let primary_range = self.ranges[self.primary.min(self.ranges.len() - 1)];
        self.ranges.sort_by_key(|r| (r.from(), r.to()));
        let mut merged: Vec<Range> = Vec::with_capacity(self.ranges.len());
        for range in self.ranges.drain(..) {
            match merged.last_mut() {
                Some(last) if last.overlaps(&range) => *last = last.merge(&range),
                _ => merged.push(range),
            }
        }
        self.ranges = merged;
        // The primary is whichever merged range now contains the old primary's head.
        self.primary = self.ranges.iter().position(|r| r.from() <= primary_range.head && primary_range.head < r.to()).unwrap_or(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_overlapping_ranges() {
        let sel = Selection::from_ranges(vec![Range::new(5, 9), Range::new(0, 3), Range::new(2, 6)], 0);
        assert_eq!(sel.ranges().len(), 1);
        assert_eq!(sel.primary(), Range::new(0, 9));
    }

    #[test]
    fn keeps_disjoint_ranges_sorted() {
        let sel = Selection::from_ranges(vec![Range::new(10, 12), Range::new(0, 3)], 0);
        assert_eq!(sel.ranges()[0], Range::new(0, 3));
        assert_eq!(sel.primary(), Range::new(10, 12));
    }
}
