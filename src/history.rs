//! Edits are expressed as transactions: a sorted list of non-overlapping changes in
//! the coordinates of the text *before* the transaction. Applying a transaction
//! yields its inverse, which is what undo uses.

use ropey::Rope;

use crate::selection::Selection;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub from: usize,
    pub to: usize,
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct Transaction {
    changes: Vec<Change>,
}

impl Transaction {
    pub fn new() -> Transaction {
        Transaction::default()
    }

    pub fn insert(&mut self, at: usize, text: impl Into<String>) {
        self.changes.push(Change { from: at, to: at, text: text.into() });
    }

    pub fn delete(&mut self, from: usize, to: usize) {
        if from < to {
            self.changes.push(Change { from, to, text: String::new() });
        }
    }

    pub fn replace(&mut self, from: usize, to: usize, text: impl Into<String>) {
        self.changes.push(Change { from, to, text: text.into() });
    }

    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    pub fn changes(&self) -> &[Change] {
        &self.changes
    }

    /// Changes sorted by position. Callers must not produce overlapping changes.
    fn sort(&mut self) {
        self.changes.sort_by_key(|c| (c.from, c.to));
    }

    /// Apply to `text`, returning the inverse transaction (in post-change coordinates).
    pub fn apply(mut self, text: &mut Rope) -> Transaction {
        self.sort();
        let mut inverse = Transaction::new();
        let mut delta: isize = 0;
        for change in &self.changes {
            let from = (change.from as isize + delta) as usize;
            let to = (change.to as isize + delta) as usize;
            let removed: String = text.slice(from..to).to_string();
            if from < to {
                text.remove(from..to);
            }
            if !change.text.is_empty() {
                text.insert(from, &change.text);
            }
            let inserted_len = change.text.chars().count();
            inverse.changes.push(Change { from, to: from + inserted_len, text: removed });
            delta += inserted_len as isize - (to - from) as isize;
        }
        inverse
    }

    /// Where a character position ends up after this transaction is applied.
    /// A position sitting exactly where text is inserted moves after it.
    pub fn map(&self, pos: usize) -> usize {
        let mut delta: isize = 0;
        for change in &self.changes {
            let inserted = change.text.chars().count() as isize;
            let removed = (change.to - change.from) as isize;
            if change.to <= pos {
                // Change lies entirely before `pos`.
                delta += inserted - removed;
            } else if change.from < pos {
                // `pos` is inside the deleted region: it lands at the change start.
                return (change.from as isize + delta) as usize;
            } else {
                break;
            }
        }
        (pos as isize + delta) as usize
    }

    pub fn map_selection(&self, selection: &Selection) -> Selection {
        selection.transform(|r| crate::selection::Range::new(self.map(r.anchor), self.map(r.head)))
    }
}

/// One undoable step. Insert-mode typing accumulates several transactions into one
/// step so that a single `u` undoes a whole burst of typing.
#[derive(Debug, Clone)]
pub struct Step {
    /// Pairs of (forward, inverse) transactions in the order they were applied.
    pub edits: Vec<(Transaction, Transaction)>,
    pub selection_before: Selection,
    pub selection_after: Selection,
}

#[derive(Debug, Default)]
pub struct History {
    undo: Vec<Step>,
    redo: Vec<Step>,
    /// Step still being built (while in insert mode).
    pending: Option<Step>,
    /// Number of steps in `undo` when the buffer was last saved, if that state is
    /// still reachable. Used to know whether the buffer is modified.
    saved_at: Option<usize>,
}

impl History {
    pub fn new() -> History {
        History { saved_at: Some(0), ..Default::default() }
    }

    /// Record an edit that was just applied. Consecutive edits are merged into the
    /// pending step until `commit` is called.
    pub fn record(&mut self, forward: Transaction, inverse: Transaction, before: &Selection, after: &Selection) {
        self.redo.clear();
        match &mut self.pending {
            Some(step) => {
                step.edits.push((forward, inverse));
                step.selection_after = after.clone();
            }
            None => {
                self.pending =
                    Some(Step { edits: vec![(forward, inverse)], selection_before: before.clone(), selection_after: after.clone() });
            }
        }
        if let Some(saved) = self.saved_at
            && saved > self.undo.len()
        {
            self.saved_at = None;
        }
    }

    /// Close the pending step so the next edit starts a new one.
    pub fn commit(&mut self) {
        if let Some(step) = self.pending.take() {
            self.undo.push(step);
        }
    }

    pub fn undo(&mut self, text: &mut Rope) -> Option<Selection> {
        self.commit();
        let step = self.undo.pop()?;
        for (_, inverse) in step.edits.iter().rev() {
            inverse.clone().apply(text);
        }
        let selection = step.selection_before.clone();
        self.redo.push(step);
        Some(selection)
    }

    pub fn redo(&mut self, text: &mut Rope) -> Option<Selection> {
        self.commit();
        let step = self.redo.pop()?;
        for (forward, _) in &step.edits {
            forward.clone().apply(text);
        }
        let selection = step.selection_after.clone();
        self.undo.push(step);
        Some(selection)
    }

    pub fn mark_saved(&mut self) {
        self.commit();
        self.saved_at = Some(self.undo.len());
    }

    pub fn is_modified(&self) -> bool {
        self.pending.is_some() || self.saved_at != Some(self.undo.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_and_inverse_round_trip() {
        let mut text = Rope::from_str("hello world");
        let mut tx = Transaction::new();
        tx.replace(0, 5, "goodbye");
        tx.delete(6, 11);
        let inverse = tx.apply(&mut text);
        assert_eq!(text.to_string(), "goodbye ");
        inverse.apply(&mut text);
        assert_eq!(text.to_string(), "hello world");
    }

    #[test]
    fn maps_positions_through_changes() {
        let mut tx = Transaction::new();
        tx.insert(2, "ab");
        tx.delete(5, 8);
        tx.sort();
        assert_eq!(tx.map(1), 1);
        assert_eq!(tx.map(2), 4);
        assert_eq!(tx.map(6), 7);
        assert_eq!(tx.map(9), 8);
    }

    #[test]
    fn undo_redo() {
        let mut text = Rope::from_str("abc");
        let mut history = History::new();
        let mut tx = Transaction::new();
        tx.insert(3, "d");
        let inverse = tx.clone().apply(&mut text);
        history.record(tx, inverse, &Selection::point(3), &Selection::point(4));
        assert!(history.is_modified());
        history.undo(&mut text);
        assert_eq!(text.to_string(), "abc");
        assert!(!history.is_modified());
        history.redo(&mut text);
        assert_eq!(text.to_string(), "abcd");
    }
}
