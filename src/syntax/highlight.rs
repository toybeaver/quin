//! Regex-based, line-by-line highlighter.
//!
//! Highlighting a line needs only the text of that line and the state left by the
//! previous one (which regions are still open). `HighlightCache` stores the state
//! at the end of every line of a buffer so that only lines after an edit need
//! recomputing, and stops early once the recomputed states converge with the old
//! ones.

use ropey::Rope;

use super::{Embed, LangId, Registry, Rule, ScopeId};

/// One open region. `lang` + `rule` identify the region rule; `embed` is the
/// language whose rules apply inside, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    pub lang: LangId,
    pub rule: usize,
    pub embed: Option<LangId>,
}

/// Highlighter state carried from one line to the next.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineState {
    pub stack: Vec<Frame>,
}

/// A highlighted piece of a line, as byte offsets into the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub scope: ScopeId,
}

/// Scope, end pattern and `keep-end` flag of the region we are currently inside.
type RegionContext<'a> = (ScopeId, &'a regex::Regex, bool);

#[derive(Debug, Clone, Copy)]
enum Found {
    RegionEnd,
    Rule(LangId, usize),
}

/// Cached search result for one rule on the current line: `None` = not searched
/// yet; `Some(None)` = no match in the rest of the line; `Some(Some((start, end)))`.
type CachedMatch = Option<Option<(usize, usize)>>;

/// Reusable scratch space: remembers, per rule, the next match found on the
/// current line so a rule is not re-run at every position.
#[derive(Default)]
pub struct Highlighter {
    /// Indexed by `[language][rule]`.
    cache: Vec<Vec<CachedMatch>>,
    /// Regions that cover some of the last highlighted line, outermost first:
    /// those open at its start (unless a `keep-end` closed them before its
    /// first character) and those that opened on it. Line decorations (code
    /// block background, quote bar, centering, tables) are read from here.
    pub line_regions: Vec<Frame>,
}

impl Highlighter {
    pub fn new() -> Highlighter {
        Highlighter::default()
    }

    fn reset_cache(&mut self, registry: &Registry) {
        if self.cache.len() < registry.languages.len() {
            self.cache.resize_with(registry.languages.len(), Vec::new);
        }
        for (i, per_rule) in self.cache.iter_mut().enumerate() {
            let rule_count = registry.languages[i].rules.len();
            per_rule.clear();
            per_rule.resize(rule_count, None);
        }
    }

    /// Next match of a rule's start pattern at or after `pos`, using the cache.
    fn next_match(&mut self, registry: &Registry, lang: LangId, rule: usize, line: &str, pos: usize) -> Option<(usize, usize)> {
        let slot = &mut self.cache[lang.0][rule];
        if let Some(cached) = *slot {
            match cached {
                None => return None,
                Some((start, end)) if start >= pos => return Some((start, end)),
                _ => {}
            }
        }
        let regex = match &registry.get(lang).rules[rule] {
            Rule::Match { regex, .. } => regex,
            Rule::Region { start, .. } => start,
        };
        // Zero-width matches are useless for a start pattern and would loop forever.
        let mut from = pos;
        let found = loop {
            match regex.find_at(line, from) {
                Some(m) if m.start() == m.end() => {
                    from = next_char_boundary(line, m.end());
                    if from > line.len() {
                        break None;
                    }
                }
                Some(m) => break Some((m.start(), m.end())),
                None => break None,
            }
        };
        *slot = Some(found);
        found
    }

    /// Highlight one line (without its trailing newline). `state` is the state at
    /// the start of the line and is updated to the state at its end.
    pub fn highlight_line(&mut self, registry: &Registry, lang: LangId, line: &str, state: &mut LineState, out: &mut Vec<Span>) {
        out.clear();
        self.reset_cache(registry);
        self.line_regions.clear();
        self.line_regions.extend_from_slice(&state.stack);
        let mut pos = 0;
        loop {
            // Rules that apply here and, inside a region, its end pattern and scope.
            let (rules, region): (Vec<(LangId, usize)>, Option<RegionContext<'_>>) = match state.stack.last() {
                None => (registry.get(lang).top.iter().map(|&r| (lang, r)).collect(), None),
                Some(frame) => match &registry.get(frame.lang).rules[frame.rule] {
                    Rule::Region { scope, end, inner, keep_end, .. } => {
                        let mut rules: Vec<(LangId, usize)> = inner.iter().map(|&r| (frame.lang, r)).collect();
                        if let Some(embedded) = frame.embed {
                            rules.extend(registry.get(embedded).top.iter().map(|&r| (embedded, r)));
                        }
                        (rules, Some((*scope, end, *keep_end)))
                    }
                    Rule::Match { .. } => unreachable!("only regions are pushed on the stack"),
                },
            };

            let mut best: Option<(usize, usize, Found)> = None;
            if let Some((_, end, _)) = region
                && let Some(m) = end.find_at(line, pos)
            {
                best = Some((m.start(), m.end(), Found::RegionEnd));
            }
            for (rule_lang, rule) in rules {
                if let Some((start, end)) = self.next_match(registry, rule_lang, rule, line, pos)
                    && best.is_none_or(|(best_start, _, _)| start < best_start)
                {
                    best = Some((start, end, Found::Rule(rule_lang, rule)));
                }
            }

            let Some((start, end, found)) = best else {
                if let Some((scope, _, _)) = region {
                    push_span(out, pos, line.len(), scope);
                }
                break;
            };
            if let Some((scope, _, _)) = region {
                push_span(out, pos, start, scope);
            }
            match found {
                Found::RegionEnd => {
                    let (scope, _, keep_end) = region.expect("inside a region");
                    let closed = state.stack.pop();
                    if keep_end {
                        // The outer rules look at the matched text again. This
                        // cannot loop: every region start consumes text.
                        if start == 0 && closed.is_some() && self.line_regions.last() == closed.as_ref() {
                            self.line_regions.pop();
                        }
                        pos = start;
                        continue;
                    }
                    push_span(out, start, end, scope);
                }
                Found::Rule(rule_lang, rule) => match &registry.get(rule_lang).rules[rule] {
                    Rule::Match { scope, regex, captures } => {
                        push_match(out, line, start, end, *scope, regex, captures);
                    }
                    Rule::Region { scope, start: start_regex, start_captures, embed, .. } => {
                        push_match(out, line, start, end, *scope, start_regex, start_captures);
                        let embed = match embed {
                            None => None,
                            Some(Embed::Language(name)) => registry.find(name),
                            Some(Embed::Capture(group)) => start_regex
                                .captures_at(line, start)
                                .and_then(|c| c.get(*group))
                                .and_then(|m| registry.find(m.as_str().trim())),
                        };
                        state.stack.push(Frame { lang: rule_lang, rule, embed });
                        self.line_regions.push(Frame { lang: rule_lang, rule, embed });
                    }
                },
            }
            pos = end;
            if pos > line.len() {
                break;
            }
            // Keep looping at the very end of the line: a region may still close
            // on a zero-width `$` match.
        }
    }
}

fn next_char_boundary(s: &str, mut i: usize) -> usize {
    i += 1;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

fn push_span(out: &mut Vec<Span>, start: usize, end: usize, scope: ScopeId) {
    if start < end {
        out.push(Span { start, end, scope });
    }
}

/// Emit a match as its base scope, with capture groups painted over it.
fn push_match(
    out: &mut Vec<Span>,
    line: &str,
    start: usize,
    end: usize,
    scope: ScopeId,
    regex: &regex::Regex,
    captures: &[(usize, ScopeId)],
) {
    if captures.is_empty() {
        push_span(out, start, end, scope);
        return;
    }
    let mut segments: Vec<Span> = vec![Span { start, end, scope }];
    if let Some(caps) = regex.captures_at(line, start) {
        for (group, capture_scope) in captures {
            let Some(m) = caps.get(*group) else { continue };
            if m.start() == m.end() {
                continue;
            }
            paint(&mut segments, Span { start: m.start(), end: m.end(), scope: *capture_scope });
        }
    }
    out.extend(segments);
}

/// Overlay `span` on a sorted list of non-overlapping spans.
fn paint(segments: &mut Vec<Span>, span: Span) {
    let mut result = Vec::with_capacity(segments.len() + 2);
    for seg in segments.drain(..) {
        if seg.end <= span.start || seg.start >= span.end {
            result.push(seg);
            continue;
        }
        if seg.start < span.start {
            result.push(Span { start: seg.start, end: span.start, scope: seg.scope });
        }
        if seg.end > span.end {
            result.push(Span { start: span.end, end: seg.end, scope: seg.scope });
        }
    }
    result.push(span);
    result.sort_by_key(|s| s.start);
    *segments = result;
}

/// Per-buffer memory of the highlighter state at the end of each line.
#[derive(Debug, Default)]
pub struct HighlightCache {
    /// `states[i]` is the state at the end of line `i`.
    states: Vec<LineState>,
    /// `states[..valid]` are known to be correct.
    valid: usize,
    /// Lines before this one may have changed since the states were computed;
    /// from here on, old states can be trusted again once they match.
    dirty_end: usize,
}

impl HighlightCache {
    pub fn new() -> HighlightCache {
        HighlightCache::default()
    }

    /// Forget everything (language changed, file reloaded...).
    pub fn reset(&mut self) {
        self.states.clear();
        self.valid = 0;
        self.dirty_end = 0;
    }

    /// Note that lines `first..=last` (new coordinates) changed, and that the
    /// buffer grew or shrank by `delta` lines.
    pub fn edited(&mut self, first: usize, last: usize, delta: isize) {
        if self.states.is_empty() {
            return;
        }
        let first = first.min(self.states.len());
        if delta < 0 {
            let removed = ((-delta) as usize).min(self.states.len() - first);
            self.states.drain(first..first + removed);
        } else if delta > 0 {
            let at = first.min(self.states.len());
            for _ in 0..delta {
                self.states.insert(at, LineState::default());
            }
        }
        self.valid = self.valid.min(first);
        if self.dirty_end >= first {
            self.dirty_end = (self.dirty_end as isize + delta).max(0) as usize;
        }
        self.dirty_end = self.dirty_end.max(last + 1);
    }

    /// Make sure the states of all lines up to and including `line` are valid.
    pub fn ensure(&mut self, line: usize, text: &Rope, lang: LangId, registry: &Registry, highlighter: &mut Highlighter) {
        let line_count = text.len_lines();
        if self.states.len() != line_count {
            // Out of step with the text (fresh cache, or the buffer was replaced):
            // nothing stored can be trusted for convergence.
            self.states.resize(line_count, LineState::default());
            self.valid = self.valid.min(line_count);
            self.dirty_end = line_count;
        }
        let line = line.min(line_count - 1);
        if line < self.valid {
            return;
        }
        let mut spans = Vec::new();
        let mut state = if self.valid == 0 { LineState::default() } else { self.states[self.valid - 1].clone() };
        for i in self.valid..=line {
            let line_text = line_without_newline(text, i);
            highlighter.highlight_line(registry, lang, &line_text, &mut state, &mut spans);
            if i >= self.dirty_end && self.states[i] == state {
                // Converged with the previously computed states: the rest is valid.
                self.valid = line_count;
                return;
            }
            self.states[i] = state.clone();
            self.valid = i + 1;
        }
    }

    /// State at the start of `line`. Call `ensure(line)` first.
    pub fn state_before(&self, line: usize) -> LineState {
        if line == 0 { LineState::default() } else { self.states.get(line - 1).cloned().unwrap_or_default() }
    }
}

/// Text of a line without its trailing newline, as an owned string.
pub fn line_without_newline(text: &Rope, line: usize) -> String {
    let slice = text.line(line);
    let mut s: String = slice.to_string();
    while s.ends_with('\n') || s.ends_with('\r') {
        s.pop();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::format;

    fn registry(source: &str) -> Registry {
        let mut registry = Registry::default();
        let lang = format::parse(source, &mut registry.scopes).unwrap();
        registry.add(lang);
        registry
    }

    fn scopes(registry: &Registry, spans: &[Span]) -> Vec<(usize, usize, String)> {
        spans.iter().map(|s| (s.start, s.end, registry.scopes.name(s.scope).to_string())).collect()
    }

    #[test]
    fn highlights_keywords_and_regions() {
        let registry = registry("name t\nkeywords keyword fn let\nmatch number \\d+\nregion comment /\\* \\*/\n  match todo TODO\n");
        let lang = LangId(0);
        let mut h = Highlighter::new();
        let mut state = LineState::default();
        let mut spans = Vec::new();
        h.highlight_line(&registry, lang, "fn x = 42; /* a TODO", &mut state, &mut spans);
        assert_eq!(
            scopes(&registry, &spans),
            vec![
                (0, 2, "keyword".into()),
                (7, 9, "number".into()),
                (11, 13, "comment".into()),
                (13, 16, "comment".into()),
                (16, 20, "todo".into()),
            ]
        );
        assert_eq!(state.stack.len(), 1);
        h.highlight_line(&registry, lang, "still */ let", &mut state, &mut spans);
        assert_eq!(scopes(&registry, &spans), vec![(0, 6, "comment".into()), (6, 8, "comment".into()), (9, 12, "keyword".into())]);
        assert!(state.stack.is_empty());
    }

    #[test]
    fn captures_split_a_match() {
        let registry = registry("name t\nmatch heading ^(#+)\\s+(.*)$ 1:conceal 2:heading.1\n");
        let mut h = Highlighter::new();
        let mut state = LineState::default();
        let mut spans = Vec::new();
        h.highlight_line(&registry, LangId(0), "# Title", &mut state, &mut spans);
        assert_eq!(scopes(&registry, &spans), vec![(0, 1, "conceal".into()), (1, 2, "heading".into()), (2, 7, "heading.1".into())]);
    }

    #[test]
    fn region_can_end_at_end_of_line() {
        let registry = registry("name t\nregion heading ^#\\s+ $\n  match strong \\*\\*[^*]+\\*\\*\n");
        let mut h = Highlighter::new();
        let mut state = LineState::default();
        let mut spans = Vec::new();
        h.highlight_line(&registry, LangId(0), "# a **b**", &mut state, &mut spans);
        assert!(state.stack.is_empty());
        assert_eq!(scopes(&registry, &spans), vec![(0, 2, "heading".into()), (2, 4, "heading".into()), (4, 9, "strong".into())]);
    }

    #[test]
    fn keep_end_leaves_the_closing_text_to_outer_rules() {
        let registry = registry("name t\nregion quote ^> ^[^>] keep-end\nmatch word \\w+\n");
        let mut h = Highlighter::new();
        let mut state = LineState::default();
        let mut spans = Vec::new();
        h.highlight_line(&registry, LangId(0), "> a", &mut state, &mut spans);
        assert_eq!(state.stack.len(), 1);
        assert_eq!(h.line_regions.len(), 1);
        // The region ends at "b" but "b" is still a word, and the line is not
        // covered by the region at all.
        h.highlight_line(&registry, LangId(0), "b", &mut state, &mut spans);
        assert!(state.stack.is_empty());
        assert!(h.line_regions.is_empty());
        assert_eq!(scopes(&registry, &spans), vec![(0, 1, "word".into())]);
    }

    #[test]
    fn cache_converges_after_edit() {
        let registry = registry("name t\nregion comment /\\* \\*/\n");
        let text = Rope::from_str("a\n/*\nb\n*/\nc\nd\n");
        let mut cache = HighlightCache::new();
        let mut h = Highlighter::new();
        cache.ensure(5, &text, LangId(0), &registry, &mut h);
        assert_eq!(cache.state_before(2).stack.len(), 1);
        assert_eq!(cache.state_before(4).stack.len(), 0);
        // Insert a line at the top; states shift by one and converge again.
        let text = Rope::from_str("x\na\n/*\nb\n*/\nc\nd\n");
        cache.edited(0, 0, 1);
        cache.ensure(6, &text, LangId(0), &registry, &mut h);
        assert_eq!(cache.state_before(3).stack.len(), 1);
        assert_eq!(cache.state_before(5).stack.len(), 0);
    }
}
