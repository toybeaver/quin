//! Cursor motions over a rope: characters, words, lines, brackets, text objects.
//! All positions are character indices.

use ropey::Rope;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharClass {
    Whitespace,
    Word,
    Punctuation,
}

pub fn char_class(c: char) -> CharClass {
    if c.is_whitespace() {
        CharClass::Whitespace
    } else if c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else {
        CharClass::Punctuation
    }
}

/// Character categories as Helix sees them: a newline is its own category so
/// that word motions stop at line ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Category {
    Whitespace,
    Eol,
    Word,
    Punctuation,
}

fn class_at(text: &Rope, pos: usize, long: bool) -> CharClass {
    let c = text.char(pos);
    if long { if c.is_whitespace() { CharClass::Whitespace } else { CharClass::Word } } else { char_class(c) }
}

fn categorize(c: char, long: bool) -> Category {
    if c == '\n' {
        Category::Eol
    } else if c.is_whitespace() {
        Category::Whitespace
    } else if long || c.is_alphanumeric() || c == '_' {
        Category::Word
    } else {
        Category::Punctuation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordMotion {
    NextWordStart,
    NextWordEnd,
    PrevWordStart,
}

fn reached_target(target: WordMotion, prev: char, next: char, long: bool) -> bool {
    let boundary = categorize(prev, long) != categorize(next, long);
    match target {
        WordMotion::NextWordStart => boundary && (next == '\n' || !next.is_whitespace()),
        WordMotion::NextWordEnd | WordMotion::PrevWordStart => boundary && (!prev.is_whitespace() || next == '\n'),
    }
}

/// Helix's word motion (`w`, `e`, `b` and their long variants), ported from
/// `helix-core/src/movement.rs`. Works on a half-open range `(anchor, head)` in
/// the direction of travel; `head` is exclusive. Returns the new range.
fn range_to_target(text: &Rope, target: WordMotion, anchor: usize, head: usize, long: bool) -> (usize, usize) {
    let len = text.len_chars();
    let backward = target == WordMotion::PrevWordStart;
    // `index` is the character the iterator will yield next.
    let mut index = head;
    let char_at = |i: usize| -> Option<char> { if i < len { Some(text.char(i)) } else { None } };
    let next_char = |index: &mut usize| -> Option<char> {
        if backward {
            if *index == 0 {
                return None;
            }
            *index -= 1;
            char_at(*index)
        } else {
            let c = char_at(*index)?;
            *index += 1;
            Some(c)
        }
    };
    let mut anchor = anchor;
    let mut head = head;
    let mut prev_ch = if backward { char_at(head) } else { head.checked_sub(1).and_then(char_at) };

    // Skip any initial newline characters.
    while let Some(ch) = next_char(&mut index) {
        if ch == '\n' {
            prev_ch = Some(ch);
            head = if backward { head.saturating_sub(1) } else { head + 1 };
        } else {
            // Un-read it.
            if backward {
                index += 1;
            } else {
                index -= 1;
            }
            break;
        }
    }
    if prev_ch == Some('\n') {
        anchor = head;
    }

    let head_start = head;
    while let Some(ch) = next_char(&mut index) {
        if prev_ch.is_none_or(|p| reached_target(target, p, ch, long)) {
            if head == head_start {
                anchor = head;
            } else {
                break;
            }
        }
        prev_ch = Some(ch);
        head = if backward { head.saturating_sub(1) } else { head + 1 };
    }
    (anchor, head)
}

/// Apply a word motion `count` times starting from the cursor at `pos`.
/// Returns the new (anchor, head) in inclusive coordinates, as Helix would
/// select them in normal mode.
pub fn word_move(text: &Rope, pos: usize, count: usize, target: WordMotion, long: bool) -> (usize, usize) {
    let len = text.len_chars();
    let backward = target == WordMotion::PrevWordStart;
    if len == 0 || (backward && pos == 0) {
        return (pos, pos);
    }
    let pos = pos.min(len);
    // Block cursor semantics: a cursor on `pos` is the half-open range covering it.
    let (mut anchor, mut head) = if backward { ((pos + 1).min(len), pos) } else { (pos, (pos + 1).min(len)) };
    if !backward && pos >= len {
        return (pos, pos);
    }
    for _ in 0..count {
        let next = range_to_target(text, target, anchor, head, long);
        if next == (anchor, head) {
            break;
        }
        (anchor, head) = next;
    }
    if backward { (anchor.saturating_sub(1).max(head), head) } else { (anchor, head.saturating_sub(1).max(anchor)) }
}

/// Character index of (line, visual column) with tabs expanded, clamped to the line.
pub fn pos_at_column(text: &Rope, line: usize, column: usize, tab_width: usize) -> usize {
    let start = text.line_to_char(line);
    let mut col = 0;
    let mut pos = start;
    for c in text.line(line).chars() {
        if c == '\n' || col >= column {
            break;
        }
        col += if c == '\t' { tab_width - col % tab_width } else { unicode_width::UnicodeWidthChar::width(c).unwrap_or(0) };
        pos += 1;
    }
    pos
}

/// Visual column of a position, with tabs expanded.
pub fn column_of(text: &Rope, pos: usize, tab_width: usize) -> usize {
    let line = text.char_to_line(pos.min(text.len_chars()));
    let start = text.line_to_char(line);
    let mut col = 0;
    for c in text.slice(start..pos).chars() {
        col += if c == '\t' { tab_width - col % tab_width } else { unicode_width::UnicodeWidthChar::width(c).unwrap_or(0) };
    }
    col
}

/// Move `n` lines up or down keeping the visual column.
pub fn move_vertically(text: &Rope, pos: usize, lines: isize, goal_col: Option<usize>, tab_width: usize) -> (usize, usize) {
    let line = text.char_to_line(pos.min(text.len_chars()));
    let col = goal_col.unwrap_or_else(|| column_of(text, pos, tab_width));
    let last_line = text.len_lines().saturating_sub(1);
    let target = (line as isize + lines).clamp(0, last_line as isize) as usize;
    (pos_at_column(text, target, col, tab_width), col)
}

/// Position of the newline that ends the line (or the text end on the last line).
pub fn line_end(text: &Rope, line: usize) -> usize {
    let start = text.line_to_char(line);
    let end = start + text.line(line).len_chars();
    if end > start && text.char(end - 1) == '\n' { end - 1 } else { end }
}

/// Find `ch` forward/backward from `pos` (exclusive). `till` stops one short.
pub fn find_char(text: &Rope, pos: usize, ch: char, forward: bool, till: bool, count: usize) -> Option<usize> {
    let len = text.len_chars();
    let mut found = 0;
    if forward {
        let mut i = pos + 1;
        while i < len {
            if text.char(i) == ch {
                found += 1;
                if found == count {
                    return Some(if till { i - 1 } else { i });
                }
            }
            i += 1;
        }
    } else {
        let mut i = pos;
        while i > 0 {
            i -= 1;
            if text.char(i) == ch {
                found += 1;
                if found == count {
                    return Some(if till { i + 1 } else { i });
                }
            }
        }
    }
    None
}

pub const BRACKETS: [(char, char); 4] = [('(', ')'), ('[', ']'), ('{', '}'), ('<', '>')];

fn bracket_pair(c: char) -> Option<(char, char, bool)> {
    for (open, close) in BRACKETS {
        if c == open {
            return Some((open, close, true));
        }
        if c == close {
            return Some((open, close, false));
        }
    }
    None
}

/// Matching bracket for the bracket at `pos` (`None` when `pos` is not on one).
pub fn matching_bracket(text: &Rope, pos: usize) -> Option<usize> {
    matching_bracket_within(text, pos, usize::MAX)
}

/// Like `matching_bracket`, but gives up after looking at `limit`
/// characters. Drawing uses this on every frame, where a bracket without a
/// partner (or a `>` starting a Markdown quote) must not scan the whole file.
pub fn matching_bracket_within(text: &Rope, pos: usize, limit: usize) -> Option<usize> {
    if pos >= text.len_chars() {
        return None;
    }
    let (open, close, forward) = bracket_pair(text.char(pos))?;
    // Walking away from the bracket under the cursor, the bracket of the
    // same kind nests and the other one unnests.
    let (same, other) = if forward { (open, close) } else { (close, open) };
    let chars: Box<dyn Iterator<Item = char>> =
        if forward { Box::new(text.chars_at(pos)) } else { Box::new(text.chars_at(pos + 1).reversed()) };
    let mut depth = 0usize;
    for (i, c) in chars.take(limit).enumerate() {
        if c == same {
            depth += 1;
        } else if c == other {
            depth -= 1;
            if depth == 0 {
                return Some(if forward { pos + i } else { pos - i });
            }
        }
    }
    None
}

/// `mm`: the bracket matching the one under the cursor, or, when the cursor is
/// not on a bracket, the closing bracket of the innermost `()`, `[]` or `{}`
/// pair around it.
pub fn goto_matching_bracket(text: &Rope, pos: usize) -> Option<usize> {
    if let Some(target) = matching_bracket(text, pos) {
        return Some(target);
    }
    let mut best: Option<(usize, usize)> = None;
    for (open, close) in BRACKETS.iter().filter(|(open, _)| *open != '<') {
        if let Some((o, c)) = surrounding_pair(text, pos, *open, *close, 1)
            && best.is_none_or(|(bo, _)| o > bo)
        {
            best = Some((o, c));
        }
    }
    best.map(|(_, close)| close)
}

/// Find the innermost pair of `open`/`close` surrounding `pos` (used by `mi(`,
/// `ma(`, `md(`...). Returns (open position, close position).
pub fn surrounding_pair(text: &Rope, pos: usize, open: char, close: char, count: usize) -> Option<(usize, usize)> {
    let len = text.len_chars();
    if len == 0 {
        return None;
    }
    let pos = pos.min(len - 1);
    if open == close {
        // Quotes: nearest on each side, within the line.
        let line = text.char_to_line(pos);
        let start = text.line_to_char(line);
        let end = line_end(text, line);
        let mut left = None;
        let mut i = pos;
        loop {
            if text.char(i) == open && (i != pos || true) {
                left = Some(i);
                break;
            }
            if i == start {
                break;
            }
            i -= 1;
        }
        let left = left?;
        let right = ((left + 1)..end).find(|&i| text.char(i) == close)?;
        return Some((left, right));
    }
    // Walk left counting depth to find the opening bracket.
    let mut depth = 0usize;
    let mut found = 0;
    let mut i = pos as isize;
    let mut open_pos = None;
    while i >= 0 {
        let c = text.char(i as usize);
        if c == close && (i as usize) != pos {
            depth += 1;
        } else if c == open {
            if depth == 0 {
                found += 1;
                if found == count {
                    open_pos = Some(i as usize);
                    break;
                }
            } else {
                depth -= 1;
            }
        }
        i -= 1;
    }
    let open_pos = open_pos?;
    let close_pos = matching_bracket(text, open_pos)?;
    if close_pos < pos {
        return None;
    }
    Some((open_pos, close_pos))
}

/// Range of the word under `pos` (inclusive end).
pub fn word_at(text: &Rope, pos: usize, long: bool) -> Option<(usize, usize)> {
    let len = text.len_chars();
    if pos >= len {
        return None;
    }
    let class = class_at(text, pos, long);
    if class == CharClass::Whitespace {
        return None;
    }
    let mut start = pos;
    while start > 0 && class_at(text, start - 1, long) == class {
        start -= 1;
    }
    let mut end = pos;
    while end + 1 < len && class_at(text, end + 1, long) == class {
        end += 1;
    }
    Some((start, end))
}

/// Range (inclusive end) of the paragraph around `pos`: lines separated by blank lines.
pub fn paragraph_at(text: &Rope, pos: usize) -> (usize, usize) {
    let line = text.char_to_line(pos.min(text.len_chars()));
    let is_blank = |l: usize| text.line(l).chars().all(|c| c.is_whitespace());
    let mut first = line;
    while first > 0 && !is_blank(first - 1) {
        first -= 1;
    }
    let mut last = line;
    while last + 1 < text.len_lines() && !is_blank(last + 1) {
        last += 1;
    }
    (text.line_to_char(first), line_end(text, last).max(text.line_to_char(first)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_motions() {
        let text = Rope::from_str("foo bar.baz  qux\n\nend");
        assert_eq!(word_move(&text, 0, 1, WordMotion::NextWordStart, false), (0, 3));
        assert_eq!(word_move(&text, 3, 1, WordMotion::NextWordStart, false), (4, 6));
        assert_eq!(word_move(&text, 4, 1, WordMotion::NextWordStart, true), (4, 12));
        assert_eq!(word_move(&text, 0, 1, WordMotion::NextWordEnd, false), (0, 2));
        assert_eq!(word_move(&text, 2, 1, WordMotion::NextWordEnd, false), (3, 6));
        assert_eq!(word_move(&text, 13, 1, WordMotion::PrevWordStart, false), (12, 8));
        assert_eq!(word_move(&text, 0, 2, WordMotion::NextWordStart, false), (4, 6));
    }

    #[test]
    fn brackets_and_pairs() {
        let text = Rope::from_str("f(a, (b)) x");
        assert_eq!(matching_bracket(&text, 1), Some(8));
        assert_eq!(matching_bracket(&text, 8), Some(1));
        assert_eq!(matching_bracket(&text, 0), None);
        assert_eq!(matching_bracket(&text, 5), Some(7));
        assert_eq!(matching_bracket(&text, 7), Some(5));
        // A bounded search stops early: a Markdown quote's `>` has no partner.
        assert_eq!(matching_bracket_within(&text, 1, 8), Some(8));
        assert_eq!(matching_bracket_within(&text, 1, 7), None);
        assert_eq!(matching_bracket_within(&text, 8, 7), None);
        let quote = Rope::from_str(&"> quoted line\n".repeat(1000));
        assert_eq!(matching_bracket_within(&quote, quote.len_chars() - 14, 100), None);
        assert_eq!(goto_matching_bracket(&text, 0), None);
        assert_eq!(goto_matching_bracket(&text, 2), Some(8), "inside a pair: its closing bracket");
        assert_eq!(goto_matching_bracket(&text, 6), Some(7), "the innermost pair wins");
        assert_eq!(surrounding_pair(&text, 6, '(', ')', 1), Some((5, 7)));
        assert_eq!(surrounding_pair(&text, 6, '(', ')', 2), Some((1, 8)));
        let quoted = Rope::from_str("say \"hi there\" now");
        assert_eq!(surrounding_pair(&quoted, 7, '"', '"', 1), Some((4, 13)));
    }

    #[test]
    fn columns_with_tabs() {
        let text = Rope::from_str("\tab\nxyz");
        assert_eq!(column_of(&text, 1, 4), 4);
        assert_eq!(pos_at_column(&text, 0, 5, 4), 2);
        assert_eq!(move_vertically(&text, 2, 1, None, 4).0, 7);
    }
}
