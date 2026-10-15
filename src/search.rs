//! Regex search inside a buffer.

use regex::Regex;
use ropey::Rope;

/// A match as a character range `[start, end)`.
pub type Match = (usize, usize);

/// Find the next match after `from` (or before it when `reverse`), wrapping
/// around the text. Returns `None` when there is no match at all.
pub fn find(text: &Rope, regex: &Regex, from: usize, reverse: bool) -> Option<Match> {
    let haystack = text.to_string();
    let from_byte = text.char_to_byte(from.min(text.len_chars()));
    let to_chars = |m: regex::Match| (text.byte_to_char(m.start()), text.byte_to_char(m.end()));
    if reverse {
        // The regex crate only searches forward: take the last match that starts
        // before `from`, else the last match in the whole text (wrap).
        let mut last_before = None;
        let mut last = None;
        for m in regex.find_iter(&haystack) {
            if m.start() < from_byte {
                last_before = Some(m);
            }
            last = Some(m);
        }
        last_before.or(last).map(to_chars)
    } else {
        let mut iter = regex.find_iter(&haystack[from_byte..]).filter(|m| m.end() > m.start() || m.start() > 0);
        if let Some(m) = iter.next() {
            return Some((text.byte_to_char(from_byte + m.start()), text.byte_to_char(from_byte + m.end())));
        }
        regex.find(&haystack).map(to_chars)
    }
}

/// All matches inside `[from, to)`.
pub fn find_all(text: &Rope, regex: &Regex, from: usize, to: usize) -> Vec<Match> {
    let slice: String = text.slice(from..to).to_string();
    let base_byte = text.char_to_byte(from);
    regex
        .find_iter(&slice)
        .filter(|m| m.end() > m.start())
        .map(|m| (text.byte_to_char(base_byte + m.start()), text.byte_to_char(base_byte + m.end())))
        .collect()
}

/// Build a case-insensitive regex unless the pattern contains an upper case letter
/// (smart case, like Helix).
pub fn compile(pattern: &str) -> Result<Regex, String> {
    let smart = if pattern.chars().any(|c| c.is_uppercase()) { "" } else { "(?i)" };
    Regex::new(&format!("{smart}{pattern}")).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_around() {
        let text = Rope::from_str("ab ab ab");
        let re = compile("ab").unwrap();
        assert_eq!(find(&text, &re, 1, false), Some((3, 5)));
        assert_eq!(find(&text, &re, 7, false), Some((0, 2)));
        assert_eq!(find(&text, &re, 3, true), Some((0, 2)));
        assert_eq!(find(&text, &re, 0, true), Some((6, 8)));
        assert_eq!(find_all(&text, &re, 0, 8).len(), 3);
    }
}
