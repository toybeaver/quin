//! Converters from other editors' grammar files to quin's `.syn` format.
//!
//! * TextMate grammars (`*.tmLanguage.json`, what VS Code uses) are regex based
//!   like ours, so most rules convert directly. Oniguruma-only constructs
//!   (lookaround, back-references) are dropped or the rule is skipped with a
//!   `# skipped` comment to fix by hand.
//! * tree-sitter grammars are real parsers and cannot be converted; instead the
//!   `highlights.scm` query file is mined for keyword lists and a few generic
//!   rules are added, giving a starting point to edit by hand.
//!
//! Both produce text that parses with `super::format::parse`.

mod json;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use regex::Regex;

use json::Json;

/// Scopes quin themes know about (see `assets/themes/dark.theme`).
const KNOWN_SCOPES: &[&str] = &[
    "comment",
    "keyword",
    "operator",
    "type",
    "function",
    "constant",
    "number",
    "string",
    "char",
    "regex",
    "attribute",
    "annotation",
    "variable",
    "property",
    "namespace",
    "label",
    "tag",
    "punctuation",
    "preprocessor",
    "special",
    "heading",
    "strong",
    "emphasis",
    "strike",
    "code",
    "link",
    "quote",
    "diff",
];

// ---------------------------------------------------------------------------
// Output helpers shared by both converters
// ---------------------------------------------------------------------------

/// Quote an argument so that `format::tokenize` reads it back verbatim.
fn quote(arg: &str) -> String {
    let needs_quotes = arg.is_empty() || arg.contains(char::is_whitespace) || arg.starts_with('\'') || arg.starts_with('"');
    if !needs_quotes {
        return arg.to_string();
    }
    if !arg.contains('\'') {
        return format!("'{arg}'");
    }
    if !arg.contains('"') {
        return format!("\"{arg}\"");
    }
    // Both quote kinds inside: spell the single quotes as a regex escape instead.
    format!("'{}'", arg.replace('\'', r"\x27"))
}

fn comment_line(out: &mut String, indent: &str, text: &str) {
    for line in text.lines() {
        out.push_str(indent);
        out.push_str("# ");
        out.push_str(line);
        out.push('\n');
    }
}

// ---------------------------------------------------------------------------
// Regex rewriting: Oniguruma (TextMate) -> Rust `regex`
// ---------------------------------------------------------------------------

/// A rewritten pattern plus, for every capture group of the original pattern,
/// its new number (or `None` if it was inside a dropped lookaround).
struct Rewritten {
    pattern: String,
    groups: Vec<Option<usize>>,
    /// When leading/trailing lookarounds were turned into consumed context, the
    /// original pattern lives in this capture group and should get the scope.
    base_group: Option<usize>,
}

#[cfg(test)]
pub fn rewrite_regex(pattern: &str) -> Result<String, String> {
    rewrite(pattern).map(|r| r.pattern)
}

/// What a leading lookbehind / trailing lookahead becomes.
enum Context {
    Dropped,
    /// `(?<!\w)` / `(?!\w)`: a plain word boundary.
    Boundary,
    /// Positive lookaround: match its text for real (it just gets no scope).
    Consume(String),
}

fn rewrite(pattern: &str) -> Result<Rewritten, String> {
    let pattern = single_line(pattern);
    let (flags, pattern) = split_flags(&pattern);
    // In extended mode the whitespace around the pattern means nothing.
    let pattern = if flags.contains('x') { pattern.trim().to_string() } else { pattern };
    let (prefix, core, suffix) = split_context(&pattern)?;
    let core = rewrite_inner(&core)?;
    let prefix_consumed = match &prefix {
        Context::Consume(text) => Some(rewrite_inner(text)?),
        _ => None,
    };
    let suffix_consumed = match &suffix {
        Context::Consume(text) => Some(rewrite_inner(text)?),
        _ => None,
    };

    let mut out = rewrite_inner(&flags)?.pattern;
    let mut groups = Vec::new();
    // Number of capture groups emitted so far, to renumber each piece.
    let mut emitted = 0;
    let mut base_group = None;
    if matches!(prefix, Context::Boundary) {
        out.push_str(r"\b");
    }
    if let Some(piece) = &prefix_consumed {
        out.push_str(&format!("(?:{})", piece.pattern));
        append_groups(&mut groups, &piece.groups, &mut emitted);
    }
    let wrap = (prefix_consumed.is_some() || suffix_consumed.is_some()) && !core.pattern.is_empty();
    if wrap {
        out.push('(');
        emitted += 1;
        base_group = Some(emitted);
    }
    out.push_str(&core.pattern);
    append_groups(&mut groups, &core.groups, &mut emitted);
    if wrap {
        out.push(')');
    }
    if let Some(piece) = &suffix_consumed {
        out.push_str(&format!("(?:{})", piece.pattern));
        append_groups(&mut groups, &piece.groups, &mut emitted);
    }
    if matches!(suffix, Context::Boundary) {
        out.push_str(r"\b");
    }
    Ok(Rewritten { pattern: out, groups, base_group })
}

fn append_groups(groups: &mut Vec<Option<usize>>, piece: &[Option<usize>], emitted: &mut usize) {
    for group in piece {
        groups.push(group.map(|n| n + *emitted));
    }
    *emitted += piece.iter().filter(|g| g.is_some()).count();
}

/// Peel lookbehinds off the start and lookaheads off the end of a pattern.
/// They are the one place where a lookaround can be approximated: by matching
/// the context for real, or by a word boundary for the `(?!\w)` idiom.
fn split_context(pattern: &str) -> Result<(Context, String, Context), String> {
    if has_top_level_alternation(pattern) {
        // `a|(?=b)`: the lookaround belongs to one branch only; leave it to the
        // inner rewriter, which drops it.
        return Ok((Context::Dropped, pattern.to_string(), Context::Dropped));
    }
    let mut chars: Vec<char> = pattern.chars().collect();
    let mut prefix = Context::Dropped;
    while chars.starts_with(&['(', '?', '<', '=']) || chars.starts_with(&['(', '?', '<', '!']) {
        let end = matching_paren(&chars, 0).ok_or("unbalanced parentheses")?;
        let body: String = chars[4..end].iter().collect();
        let context = lookaround_context(chars[3] == '=', &body);
        if !matches!(context, Context::Dropped) {
            prefix = context;
        }
        chars.drain(..=end);
    }
    let mut suffix = Context::Dropped;
    while let Some(start) = trailing_group_start(&chars) {
        if !(chars[start..].starts_with(&['(', '?', '=']) || chars[start..].starts_with(&['(', '?', '!'])) {
            break;
        }
        let body: String = chars[start + 3..chars.len() - 1].iter().collect();
        let context = lookaround_context(chars[start + 2] == '=', &body);
        if !matches!(context, Context::Dropped) {
            suffix = context;
        }
        chars.truncate(start);
    }
    let core: String = chars.iter().collect();
    // A word boundary only makes sense next to a word character.
    if matches!(prefix, Context::Boundary) && !starts_with_word_char(&core) {
        prefix = Context::Dropped;
    }
    if matches!(suffix, Context::Boundary) && !ends_with_word_char(&core) {
        suffix = Context::Dropped;
    }
    Ok((prefix, core, suffix))
}

/// A `|` at depth zero, outside character classes.
fn has_top_level_alternation(pattern: &str) -> bool {
    let chars: Vec<char> = pattern.chars().collect();
    let mut depth = 0;
    let mut in_class = false;
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 1,
            '[' if !in_class => in_class = true,
            ']' if in_class => in_class = false,
            '(' if !in_class => depth += 1,
            ')' if !in_class => depth -= 1,
            '|' if !in_class && depth == 0 => return true,
            _ => {}
        }
        i += 1;
    }
    false
}

/// Leading inline flags like `(?x)` apply to the whole pattern, so they must
/// stay in front of any context we wrap around it.
fn split_flags(pattern: &str) -> (String, String) {
    if let Some(rest) = pattern.strip_prefix("(?")
        && let Some(end) = rest.find(')')
    {
        let flags = &rest[..end];
        if !flags.is_empty() && flags.chars().all(|c| c.is_ascii_alphabetic() || c == '-') {
            return (pattern[..end + 3].to_string(), pattern[end + 3..].to_string());
        }
    }
    (String::new(), pattern.to_string())
}

fn lookaround_context(positive: bool, body: &str) -> Context {
    if positive {
        Context::Consume(body.to_string())
    } else if body.contains("\\w") {
        Context::Boundary
    } else {
        Context::Dropped
    }
}

/// Start of the group that closes at the very end of the pattern, if any.
fn trailing_group_start(chars: &[char]) -> Option<usize> {
    if chars.last() != Some(&')') {
        return None;
    }
    let mut i = 0;
    let mut in_class = false;
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 1,
            '[' if !in_class => in_class = true,
            ']' if in_class => in_class = false,
            '(' if !in_class && matching_paren(chars, i) == Some(chars.len() - 1) => {
                return Some(i);
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn starts_with_word_char(core: &str) -> bool {
    let trimmed = core.trim_start_matches(['(', '?', ':', '[']);
    trimmed.starts_with("\\w") || trimmed.starts_with("\\d") || trimmed.chars().next().is_some_and(|c| c.is_alphanumeric() || c == '_')
}

fn ends_with_word_char(core: &str) -> bool {
    let trimmed = core.trim_end_matches(|c| matches!(c, ')' | ']' | '+' | '*' | '?' | '}' | '0'..='9' | ',' | '{'));
    trimmed.ends_with("\\w") || trimmed.ends_with("\\d") || trimmed.chars().last().is_some_and(|c| c.is_alphanumeric() || c == '_')
}

fn rewrite_inner(pattern: &str) -> Result<Rewritten, String> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::new();
    let mut groups: Vec<Option<usize>> = Vec::new();
    let mut next_group = 1;
    let mut i = 0;
    let mut in_class = false;
    // True right after `[` or `[^`, where `]` is a literal.
    let mut class_start = false;
    // True right after a quantifier, where a `+` would be a possessive marker.
    let mut after_quantifier = false;

    while i < chars.len() {
        let c = chars[i];
        let was_after_quantifier = after_quantifier;
        after_quantifier = false;
        if in_class {
            match c {
                '\\' if i + 1 < chars.len() => {
                    match chars[i + 1] {
                        'h' => out.push_str("0-9a-fA-F"),
                        'e' => out.push_str(r"\x1b"),
                        'u' => out.push_str(&unicode_escape(&chars, i + 2)?),
                        other => {
                            out.push('\\');
                            out.push(other);
                        }
                    }
                    i += if chars[i + 1] == 'u' { 6 } else { 2 };
                    class_start = false;
                    continue;
                }
                '[' if chars.get(i + 1) == Some(&':') => {
                    // POSIX class like [:alpha:], copy through.
                    let end = find_sequence(&chars, i, &[':', ']']).ok_or("unterminated POSIX class")?;
                    out.extend(&chars[i..=end + 1]);
                    i = end + 2;
                    class_start = false;
                    continue;
                }
                ']' if !class_start => in_class = false,
                _ => {}
            }
            out.push(c);
            class_start = false;
            i += 1;
            continue;
        }

        match c {
            '\\' => {
                let next = *chars.get(i + 1).ok_or("trailing backslash")?;
                i += 2;
                match next {
                    '1'..='9' => return Err("back-reference".to_string()),
                    'k' if chars.get(i) == Some(&'<') => return Err("named back-reference".to_string()),
                    'g' => return Err("subexpression call".to_string()),
                    'h' => out.push_str("[0-9a-fA-F]"),
                    'H' => out.push_str("[^0-9a-fA-F]"),
                    'A' => out.push('^'),
                    'z' | 'Z' => out.push('$'),
                    // \G (end of previous match) and \K (keep) only change match
                    // extents, which highlighting can live without.
                    'G' | 'K' => {}
                    'e' => out.push_str(r"\x1b"),
                    'a' => out.push_str(r"\x07"),
                    'R' => out.push_str(r"\n"),
                    'X' => out.push('.'),
                    // Oniguruma: literal angle brackets. Rust: word boundaries.
                    '<' => out.push('<'),
                    '>' => out.push('>'),
                    'u' => {
                        out.push_str(&unicode_escape(&chars, i)?);
                        i += 4;
                    }
                    other => {
                        out.push('\\');
                        out.push(other);
                    }
                }
            }
            '[' => {
                in_class = true;
                class_start = true;
                out.push('[');
                i += 1;
                if chars.get(i) == Some(&'^') {
                    out.push('^');
                    i += 1;
                }
                continue;
            }
            '(' if chars.get(i + 1) == Some(&'?') => {
                let kind = chars.get(i + 2).copied().unwrap_or(')');
                let lookbehind = kind == '<' && matches!(chars.get(i + 3), Some('=') | Some('!'));
                if kind == '=' || kind == '!' || lookbehind || kind == '#' {
                    // Lookaround (or inline comment): drop the whole group. Capture
                    // groups inside it disappear, which shifts later numbering.
                    let end = matching_paren(&chars, i).ok_or("unbalanced parentheses")?;
                    let body: String = chars[i + 3 + usize::from(lookbehind)..end].iter().collect();
                    // "Followed by a newline" / "preceded by a line start" are
                    // exactly our `$` and `^`, since we match one line at a time.
                    if kind == '=' && (body == "\\n" || body == "$") {
                        out.push('$');
                    } else if lookbehind && chars[i + 3] == '=' && (body == "^" || body == "\\n") {
                        out.push('^');
                    }
                    for _ in 0..count_capturing_groups(&chars[i + 1..end]) {
                        groups.push(None);
                    }
                    i = end + 1;
                    continue;
                }
                if kind == '>' {
                    // Atomic group: plain non-capturing group is the closest thing.
                    out.push_str("(?:");
                    i += 3;
                    continue;
                }
                if kind == '(' {
                    return Err("conditional group".to_string());
                }
                if kind == '<' || (kind == 'P' && chars.get(i + 3) == Some(&'<')) {
                    groups.push(Some(next_group));
                    next_group += 1;
                    out.push('(');
                    i += 1;
                    continue;
                }
                // Inline flags: Oniguruma `m` means "dot matches newline", which is `s` in Rust.
                out.push_str("(?");
                i += 2;
                while i < chars.len() && chars[i] != ')' && chars[i] != ':' {
                    out.push(if chars[i] == 'm' { 's' } else { chars[i] });
                    i += 1;
                }
                continue;
            }
            '(' => {
                groups.push(Some(next_group));
                next_group += 1;
                out.push('(');
                i += 1;
                continue;
            }
            '{' => {
                if let Some(end) = repetition_end(&chars, i) {
                    let body: String = chars[i + 1..end].iter().collect();
                    // Oniguruma allows `{,n}`; Rust wants `{0,n}`.
                    out.push('{');
                    if body.starts_with(',') {
                        out.push('0');
                    }
                    out.push_str(&body);
                    out.push('}');
                    i = end + 1;
                    after_quantifier = true;
                    continue;
                }
                out.push_str(r"\{");
                i += 1;
                continue;
            }
            '}' => {
                out.push_str(r"\}");
                i += 1;
                continue;
            }
            '+' if was_after_quantifier => {
                // Possessive quantifier (`a*+`): drop the marker.
                i += 1;
                continue;
            }
            '*' | '+' | '?' => {
                out.push(c);
                i += 1;
                after_quantifier = true;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    if in_class {
        return Err("unterminated character class".to_string());
    }
    Ok(Rewritten { pattern: out, groups, base_group: None })
}

/// Our format is line based, so a pattern must not contain newlines. In
/// extended mode (`(?x)`) whitespace is insignificant and `#` starts a comment,
/// which must go before the newline does; elsewhere a newline becomes `\n`.
fn single_line(pattern: &str) -> String {
    if !pattern.contains('\n') {
        return pattern.to_string();
    }
    if !pattern.contains("(?x") {
        return pattern.replace('\n', "\\n").replace('\t', "\\t");
    }
    let mut out = String::new();
    let mut chars = pattern.chars().peekable();
    let mut in_class = false;
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                out.push(c);
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            }
            '[' => {
                in_class = true;
                out.push(c);
            }
            ']' => {
                in_class = false;
                out.push(c);
            }
            '#' if !in_class => {
                // Comment: drop up to the end of the line.
                while chars.peek().is_some_and(|&n| n != '\n') {
                    chars.next();
                }
            }
            '\n' | '\t' => out.push(' '),
            _ => out.push(c),
        }
    }
    out
}

/// `\uXXXX` (four hex digits starting at `at`) as a Rust `\x{XXXX}` escape.
fn unicode_escape(chars: &[char], at: usize) -> Result<String, String> {
    let digits: String = chars.get(at..at + 4).ok_or("bad \\u escape")?.iter().collect();
    if !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("bad \\u escape".to_string());
    }
    Ok(format!(r"\x{{{digits}}}"))
}

/// End index of a `{n}`, `{n,}`, `{n,m}` or `{,m}` repetition starting at `at`.
fn repetition_end(chars: &[char], at: usize) -> Option<usize> {
    let mut i = at + 1;
    let mut digits = 0;
    let mut commas = 0;
    while i < chars.len() {
        match chars[i] {
            '0'..='9' => digits += 1,
            ',' => commas += 1,
            '}' => return if digits > 0 && commas <= 1 { Some(i) } else { None },
            _ => return None,
        }
        i += 1;
    }
    None
}

fn find_sequence(chars: &[char], from: usize, needle: &[char]) -> Option<usize> {
    (from..chars.len().saturating_sub(needle.len() - 1)).find(|&i| chars[i..i + needle.len()] == *needle)
}

/// Index of the `)` matching the `(` at `at`, skipping escapes and classes.
fn matching_paren(chars: &[char], at: usize) -> Option<usize> {
    let mut depth = 0;
    let mut i = at;
    let mut in_class = false;
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 1,
            '[' if !in_class => in_class = true,
            ']' if in_class => in_class = false,
            '(' if !in_class => depth += 1,
            ')' if !in_class => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn count_capturing_groups(chars: &[char]) -> usize {
    let mut count = 0;
    let mut i = 0;
    let mut in_class = false;
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 1,
            '[' if !in_class => in_class = true,
            ']' if in_class => in_class = false,
            '(' if !in_class => {
                let named = chars.get(i + 1) == Some(&'?')
                    && (chars.get(i + 2) == Some(&'P')
                        || (chars.get(i + 2) == Some(&'<') && !matches!(chars.get(i + 3), Some('=') | Some('!'))));
                if chars.get(i + 1) != Some(&'?') || named {
                    count += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    count
}

// ---------------------------------------------------------------------------
// TextMate
// ---------------------------------------------------------------------------

/// TextMate scope prefix -> quin scope. The longest matching prefix wins.
const TEXTMATE_SCOPES: &[(&str, &str)] = &[
    ("comment.block.documentation", "comment.doc"),
    ("comment.line.documentation", "comment.doc"),
    ("comment", "comment"),
    ("keyword.control.import", "keyword.import"),
    ("keyword.control.directive", "preprocessor"),
    ("keyword.control", "keyword.control"),
    ("keyword.operator", "operator"),
    ("keyword.other.unit", "number"),
    ("keyword", "keyword"),
    ("storage.type", "keyword.storage"),
    ("storage.modifier", "keyword"),
    ("storage", "keyword"),
    ("entity.name.function.macro", "function.macro"),
    ("entity.name.function", "function"),
    ("entity.name.type", "type"),
    ("entity.name.class", "type"),
    ("entity.name.struct", "type"),
    ("entity.name.enum", "type"),
    ("entity.name.namespace", "namespace"),
    ("entity.name.module", "namespace"),
    ("entity.name.import", "namespace"),
    ("entity.name.tag", "tag"),
    ("entity.name.label", "label"),
    ("entity.name.section", "heading"),
    ("entity.name", "variable"),
    ("entity.other.attribute-name", "attribute"),
    ("entity.other.inherited-class", "type"),
    ("support.type", "type"),
    ("support.class", "type"),
    ("support.function", "function.builtin"),
    ("support.constant", "constant.builtin"),
    ("support.variable", "variable.builtin"),
    ("support.macro", "function.macro"),
    ("support", "type"),
    ("constant.numeric", "number"),
    ("constant.language", "constant.builtin"),
    ("constant.character.escape", "string.escape"),
    ("constant.character", "char"),
    ("constant.other.placeholder", "string.special"),
    ("constant", "constant"),
    ("string.regexp", "regex"),
    ("string.other.link", "link.url"),
    ("string", "string"),
    ("variable.parameter", "variable.parameter"),
    ("variable.language", "variable.builtin"),
    ("variable.other.property", "property"),
    ("variable.other.member", "property"),
    ("variable.other.object.property", "property"),
    ("variable.other.constant", "constant"),
    ("variable.function", "function"),
    ("variable", "variable"),
    ("punctuation.definition.comment", "comment"),
    ("punctuation.definition.string", "string"),
    ("punctuation", "punctuation"),
    ("meta.preprocessor", "preprocessor"),
    ("meta.attribute", "attribute"),
    ("meta.annotation", "annotation"),
    ("meta.function-call", "function"),
    ("meta.tag", "tag"),
    ("meta", "text"),
    ("markup.heading", "heading"),
    ("markup.bold", "strong"),
    ("markup.italic", "emphasis"),
    ("markup.strikethrough", "strike"),
    ("markup.inline.raw", "code"),
    ("markup.raw", "code.block"),
    ("markup.fenced_code", "code.block"),
    ("markup.underline.link", "link.url"),
    ("markup.underline", "link"),
    ("markup.list", "list.marker"),
    ("markup.quote", "quote"),
    ("markup.inserted", "diff.add"),
    ("markup.deleted", "diff.remove"),
    ("markup.changed", "diff.header"),
    ("invalid", "special"),
    ("source", "text"),
    ("text", "text"),
];

/// Map a TextMate scope (`keyword.control.go`) to a quin one (`keyword.control`).
pub fn map_textmate_scope(scope: &str) -> String {
    let scope = scope.trim();
    let mut best: Option<(&str, &str)> = None;
    for (prefix, ours) in TEXTMATE_SCOPES {
        let matches = scope == *prefix || scope.starts_with(&format!("{prefix}."));
        if matches && best.is_none_or(|(p, _)| prefix.len() > p.len()) {
            best = Some((prefix, ours));
        }
    }
    if let Some((_, ours)) = best {
        return ours.to_string();
    }
    let first = scope.split('.').next().unwrap_or("");
    if KNOWN_SCOPES.contains(&first) { first.to_string() } else { "special".to_string() }
}

struct TextmateConverter<'a> {
    language: String,
    repository: HashMap<&'a str, &'a Json>,
    /// Repository names currently being expanded, to stop include cycles.
    stack: Vec<&'a str>,
    /// Repository rules already emitted at the top level / inside the current
    /// region. TextMate grammars include the same rules everywhere; emitting
    /// each one once per context keeps the output readable.
    top_visited: HashSet<&'a str>,
    inner_visited: HashSet<&'a str>,
    rules: String,
    converted: usize,
    skipped: usize,
    keyword_list: Regex,
}

/// Rules are capped so a pathological grammar cannot explode the output.
const MAX_RULES: usize = 3000;

pub fn textmate_to_syn(json: &str) -> Result<String, String> {
    let grammar = Json::parse(json).map_err(|e| format!("invalid JSON: {e}"))?;
    let scope_name = grammar.str_field("scopeName").unwrap_or("");
    let language = match grammar.str_field("name") {
        Some(name) => name.to_ascii_lowercase().replace(' ', "-"),
        None => scope_name.rsplit('.').next().unwrap_or("converted").to_ascii_lowercase(),
    };
    let repository: HashMap<&str, &Json> = grammar
        .get("repository")
        .and_then(Json::as_object)
        .map(|fields| fields.iter().map(|(k, v)| (k.as_str(), v)).collect())
        .unwrap_or_default();

    let mut conv = TextmateConverter {
        language: language.clone(),
        repository,
        stack: Vec::new(),
        top_visited: HashSet::new(),
        inner_visited: HashSet::new(),
        rules: String::new(),
        converted: 0,
        skipped: 0,
        keyword_list: Regex::new(r"^(?:\\b)?\((?:\?:)?([A-Za-z_]\w*(?:\|[A-Za-z_]\w*)*)\)(?:\\b)?$").expect("static regex"),
    };
    if let Some(patterns) = grammar.get("patterns").and_then(Json::as_array) {
        conv.emit_patterns(patterns, false);
    }

    let mut out = String::new();
    comment_line(&mut out, "", &format!("{language} — converted from the TextMate grammar `{scope_name}` by `quin convert textmate`."));
    comment_line(
        &mut out,
        "",
        &format!("{} rules converted, {} skipped. Lookarounds were dropped silently;", conv.converted, conv.skipped),
    );
    comment_line(&mut out, "", "rules marked `skipped` below need a hand-written replacement.");
    out.push('\n');
    out.push_str(&format!("name {}\n", quote(&language)));
    let extensions: Vec<String> = grammar
        .get("fileTypes")
        .and_then(Json::as_array)
        .map(|types| types.iter().filter_map(Json::as_str).map(|t| quote(t.trim_start_matches('.'))).collect())
        .unwrap_or_default();
    if !extensions.is_empty() {
        out.push_str(&format!("extensions {}\n", extensions.join(" ")));
    }
    out.push('\n');
    out.push_str(&conv.rules);
    Ok(out)
}

impl<'a> TextmateConverter<'a> {
    fn emit_patterns(&mut self, patterns: &'a [Json], indented: bool) {
        for rule in patterns {
            if self.converted + self.skipped >= MAX_RULES {
                return;
            }
            self.emit_rule(rule, indented);
        }
    }

    fn emit_rule(&mut self, rule: &'a Json, indented: bool) {
        if let Some(target) = rule.str_field("include") {
            self.emit_include(target, indented);
        } else if let Some(pattern) = rule.str_field("match") {
            self.emit_match(rule, pattern, indented);
        } else if let Some(begin) = rule.str_field("begin") {
            match rule.str_field("end") {
                Some(end) if rule.get("while").is_none() => self.emit_region(rule, begin, end, indented),
                _ => self.skip(indented, "begin/while rules are not supported", begin),
            }
        } else if let Some(patterns) = rule.get("patterns").and_then(Json::as_array) {
            self.emit_patterns(patterns, indented);
        }
    }

    fn emit_include(&mut self, target: &'a str, indented: bool) {
        if let Some(name) = target.strip_prefix('#') {
            if self.stack.contains(&name) || self.stack.len() > 24 {
                return;
            }
            let visited = if indented { &mut self.inner_visited } else { &mut self.top_visited };
            if !visited.insert(name) {
                return;
            }
            match self.repository.get(name) {
                Some(rule) => {
                    self.stack.push(name);
                    self.emit_rule(rule, indented);
                    self.stack.pop();
                }
                None => comment_line(&mut self.rules, indent(indented), &format!("include of unknown repository rule `{name}` skipped")),
            }
            return;
        }
        // `$self`, `$base` and other grammars: at the top level they would recurse
        // forever; inside a region they become `embed=` (handled by emit_region).
        if !indented && target != "$self" && target != "$base" {
            comment_line(&mut self.rules, "", &format!("include of external grammar `{target}` skipped"));
        }
    }

    fn emit_match(&mut self, rule: &'a Json, pattern: &str, indented: bool) {
        let captures = raw_captures(rule, "captures");
        let scope =
            rule.str_field("name").map(map_textmate_scope).or_else(|| captures.get(&0).cloned()).unwrap_or_else(|| "text".to_string());
        let (regex, rewritten) = match self.compile(pattern) {
            Ok(ok) => ok,
            Err(reason) => return self.skip(indented, &reason, pattern),
        };
        if regex.as_str().is_empty() {
            return self.skip(indented, "nothing left after dropping lookarounds", pattern);
        }
        if scope == "text" && captures.values().all(|s| s == "text") {
            // A `meta.*` wrapper that colors nothing would only hide other rules.
            return;
        }
        let mut captures = map_groups(&captures, &rewritten.groups);
        let mut scope = scope;
        if let Some(base) = rewritten.base_group {
            // Context matched for real: only the original pattern gets the scope.
            if scope != "text" {
                captures = format!(" {base}:{scope}{captures}");
                scope = "text".to_string();
            }
        } else if captures.is_empty()
            && let Some(words) = self.keyword_list.captures(regex.as_str()).and_then(|c| c.get(1))
        {
            // `\b(if|else|for)\b` reads better as a keyword list.
            self.rules.push_str(&format!("{}keywords {} {}\n", indent(indented), scope, words.as_str().replace('|', " ")));
            self.converted += 1;
            return;
        }
        self.rules.push_str(&format!("{}match {} {}{}\n", indent(indented), scope, quote(regex.as_str()), captures));
        self.converted += 1;
    }

    fn emit_region(&mut self, rule: &'a Json, begin: &str, end: &str, indented: bool) {
        let mut captures = raw_captures(rule, "beginCaptures");
        if captures.is_empty() {
            captures = raw_captures(rule, "captures");
        }
        // Capture 0 of the begin pattern must not become the content's scope,
        // so unlike `match` rules an unnamed region stays plain text.
        let scope =
            rule.str_field("name").or_else(|| rule.str_field("contentName")).map(map_textmate_scope).unwrap_or_else(|| "text".to_string());
        let (begin_regex, rewritten) = match self.compile(begin) {
            Ok(ok) => ok,
            Err(reason) => return self.skip(indented, &reason, begin),
        };
        let groups = rewritten.groups;
        let end_regex = match self.compile(end) {
            Ok((regex, _)) => regex,
            // Typically `\1`: the end depends on what the begin matched.
            Err(reason) => return self.skip(indented, &format!("end pattern: {reason}"), end),
        };
        if begin_regex.as_str().is_empty() {
            return self.skip(indented, "begin pattern empty after dropping lookarounds", begin);
        }
        // An end such as `(?=\s|$)` vanishes entirely; "up to the end of the
        // line" is the closest thing we can express.
        let end_text = if end_regex.as_str().is_empty() { "$" } else { end_regex.as_str() };
        let inner = rule.get("patterns").and_then(Json::as_array).unwrap_or(&[]);
        let embed = inner.iter().filter_map(|p| p.str_field("include")).find_map(|target| self.embed_name(target));
        if scope == "text" && captures.values().all(|s| s == "text") && (indented || (inner.is_empty() && embed.is_none())) {
            // Wrapper region with nothing to show (see emit_match).
            return;
        }
        let embed = embed.map(|name| format!(" embed={name}")).unwrap_or_default();
        let captures = map_groups(&captures, &groups);
        self.rules.push_str(&format!(
            "{}region {} {} {}{}{}\n",
            indent(indented),
            scope,
            quote(begin_regex.as_str()),
            quote(end_text),
            embed,
            captures
        ));
        self.converted += 1;
        // The format has one level of nesting: a top-level region gets its inner
        // rules; a region nested inside another one is emitted as a plain
        // indented region whose own inner rules are dropped (its whole extent
        // is still highlighted with its scope).
        if !indented {
            self.inner_visited.clear();
            self.emit_patterns(inner, true);
        }
    }

    /// Language to embed for an include found inside a region.
    fn embed_name(&self, target: &str) -> Option<String> {
        if target == "$self" || target == "$base" {
            return Some(self.language.clone());
        }
        if target.starts_with('#') {
            return None;
        }
        // `source.js` / `text.html.basic` -> `js` / `html`.
        let rest = target.strip_prefix("source.").or_else(|| target.strip_prefix("text."))?;
        Some(rest.split('.').next()?.to_string())
    }

    fn compile(&self, pattern: &str) -> Result<(Regex, Rewritten), String> {
        let rewritten = rewrite(pattern)?;
        let regex = Regex::new(&rewritten.pattern).map_err(|e| first_line(&e.to_string()))?;
        Ok((regex, rewritten))
    }

    fn skip(&mut self, indented: bool, reason: &str, pattern: &str) {
        let shown: String = pattern.chars().take(80).collect();
        comment_line(&mut self.rules, indent(indented), &format!("skipped ({reason}): {shown}"));
        self.skipped += 1;
    }
}

fn indent(indented: bool) -> &'static str {
    if indented { "  " } else { "" }
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").to_string()
}

/// `captures`-style object: group number -> mapped scope.
fn raw_captures(rule: &Json, key: &str) -> BTreeMap<usize, String> {
    let mut out = BTreeMap::new();
    if let Some(fields) = rule.get(key).and_then(Json::as_object) {
        for (group, value) in fields {
            if let (Ok(group), Some(name)) = (group.parse::<usize>(), value.str_field("name")) {
                out.insert(group, map_textmate_scope(name));
            }
        }
    }
    out
}

/// Capture arguments (` 1:scope 2:scope`) renumbered through the rewrite's group map.
fn map_groups(captures: &BTreeMap<usize, String>, groups: &[Option<usize>]) -> String {
    let mut out = String::new();
    for (group, scope) in captures {
        if *group == 0 {
            continue;
        }
        if let Some(Some(new)) = groups.get(group - 1) {
            out.push_str(&format!(" {new}:{scope}"));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// tree-sitter highlights.scm
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Sexp {
    List(Vec<Sexp>),
    Vector(Vec<Sexp>),
    Str(String),
    Atom(String),
    Capture(String),
}

fn parse_sexp(source: &str) -> Result<Vec<Sexp>, String> {
    let chars: Vec<char> = source.chars().collect();
    let mut i = 0;
    let items = parse_sexp_items(&chars, &mut i, None)?;
    Ok(items)
}

/// Parse items until `closer` (or the end of input when `closer` is `None`).
fn parse_sexp_items(chars: &[char], i: &mut usize, closer: Option<char>) -> Result<Vec<Sexp>, String> {
    let mut items = Vec::new();
    while *i < chars.len() {
        let c = chars[*i];
        match c {
            ';' => {
                while *i < chars.len() && chars[*i] != '\n' {
                    *i += 1;
                }
            }
            c if c.is_whitespace() => *i += 1,
            '(' | '[' => {
                *i += 1;
                let close = if c == '(' { ')' } else { ']' };
                let inner = parse_sexp_items(chars, i, Some(close))?;
                items.push(if c == '(' { Sexp::List(inner) } else { Sexp::Vector(inner) });
            }
            ')' | ']' => {
                *i += 1;
                return if Some(c) == closer { Ok(items) } else { Err(format!("unexpected `{c}`")) };
            }
            '"' => {
                *i += 1;
                let mut s = String::new();
                loop {
                    match chars.get(*i) {
                        None => return Err("unterminated string".to_string()),
                        Some('"') => break,
                        Some('\\') => {
                            s.push(match chars.get(*i + 1) {
                                Some('n') => '\n',
                                Some('t') => '\t',
                                Some(&other) => other,
                                None => return Err("unterminated string".to_string()),
                            });
                            *i += 2;
                        }
                        Some(&ch) => {
                            s.push(ch);
                            *i += 1;
                        }
                    }
                }
                *i += 1;
                items.push(Sexp::Str(s));
            }
            _ => {
                let start = *i;
                while *i < chars.len() && !chars[*i].is_whitespace() && !matches!(chars[*i], '(' | ')' | '[' | ']' | '"' | ';') {
                    *i += 1;
                }
                let word: String = chars[start..*i].iter().collect();
                items.push(match word.strip_prefix('@') {
                    Some(name) => Sexp::Capture(name.to_string()),
                    None => Sexp::Atom(word),
                });
            }
        }
    }
    if closer.is_some() {
        return Err("unbalanced parentheses".to_string());
    }
    Ok(items)
}

/// Map a tree-sitter capture (`keyword.return`) to a quin scope; `None` = ignore.
fn map_treesitter_capture(capture: &str) -> Option<String> {
    let first = capture.split('.').next().unwrap_or("");
    let scope = match capture {
        c if c.starts_with("keyword.conditional") || c.starts_with("keyword.repeat") || c.starts_with("keyword.return") => {
            "keyword.control"
        }
        c if c.starts_with("keyword.import") || first == "include" => "keyword.import",
        c if c.starts_with("keyword.operator") || first == "operator" => "operator",
        c if c.starts_with("keyword.directive") => "preprocessor",
        c if c.starts_with("type.builtin") => "type.builtin",
        c if c.starts_with("constant.builtin") || first == "boolean" => "constant.builtin",
        c if c.starts_with("variable.builtin") => "variable.builtin",
        c if c.starts_with("variable.parameter") || first == "parameter" => "variable.parameter",
        c if c.starts_with("function.builtin") => "function.builtin",
        c if c.starts_with("function.macro") => "function.macro",
        c if c.starts_with("string.escape") || first == "escape" => "string.escape",
        c if c.starts_with("comment.documentation") => "comment.doc",
        _ => match first {
            "punctuation" | "spell" | "nospell" | "none" | "markup" => return None,
            "conditional" | "repeat" | "exception" => "keyword.control",
            "keyword" | "storageclass" | "storage" => "keyword",
            "type" | "structure" => "type",
            "constant" | "character" => "constant",
            "variable" | "field" | "property" | "function" | "comment" | "string" | "number" | "attribute" | "namespace" | "label"
            | "tag" => match first {
                "field" => "property",
                other => other,
            },
            "float" => "number",
            "module" => "namespace",
            "preproc" | "define" => "preprocessor",
            "method" | "constructor" => "function",
            "text" => "text",
            other if KNOWN_SCOPES.contains(&other) => other,
            _ => "special",
        },
    };
    Some(scope.to_string())
}

#[derive(Default)]
struct TreesitterFacts {
    /// scope -> words.
    keywords: BTreeMap<String, BTreeSet<String>>,
    operators: BTreeSet<String>,
    /// (node name, scope) pairs seen as `(node) @scope`.
    nodes: BTreeSet<(String, String)>,
    literals: BTreeSet<String>,
}

impl TreesitterFacts {
    fn add_literal(&mut self, token: &str, capture: &str) {
        self.literals.insert(token.to_string());
        let Some(scope) = map_treesitter_capture(capture) else { return };
        if token.chars().any(char::is_alphanumeric) {
            self.keywords.entry(scope).or_default().insert(token.to_string());
        } else if scope == "operator" {
            self.operators.insert(token.to_string());
        }
    }

    fn walk(&mut self, items: &[Sexp]) {
        for (i, item) in items.iter().enumerate() {
            let capture = match items.get(i + 1) {
                Some(Sexp::Capture(name)) => Some(name.as_str()),
                _ => None,
            };
            match item {
                Sexp::Str(s) => {
                    if let Some(capture) = capture {
                        self.add_literal(s, capture);
                    }
                }
                Sexp::Vector(inner) => {
                    if let Some(capture) = capture {
                        for s in inner.iter().filter_map(|x| if let Sexp::Str(s) = x { Some(s.clone()) } else { None }) {
                            self.add_literal(&s, capture);
                        }
                    }
                    self.walk(inner);
                }
                Sexp::List(inner) => {
                    if let Some(Sexp::Atom(head)) = inner.first() {
                        if head == "#any-of?" || head == "#eq?" {
                            // ((identifier) @variable.builtin (#any-of? @variable.builtin "self" "super"))
                            if let Some(Sexp::Capture(name)) = inner.get(1) {
                                for s in inner[2..].iter().filter_map(|x| if let Sexp::Str(s) = x { Some(s.clone()) } else { None }) {
                                    self.add_literal(&s, name);
                                }
                            }
                        } else if let (false, Some(capture)) = (head.starts_with('#'), capture)
                            && let Some(scope) = map_treesitter_capture(capture)
                        {
                            self.nodes.insert((head.clone(), scope));
                        }
                    }
                    self.walk(inner);
                }
                _ => {}
            }
        }
    }

    fn has_node(&self, names: &[&str], scope: &str) -> bool {
        self.nodes.iter().any(|(node, s)| names.iter().any(|n| node.contains(n)) && s.starts_with(scope))
    }
}

pub fn treesitter_to_syn(name: &str, highlights_scm: &str) -> Result<String, String> {
    let items = parse_sexp(highlights_scm)?;
    let mut facts = TreesitterFacts::default();
    facts.walk(&items);
    let name = name.to_ascii_lowercase();

    let mut out = String::new();
    comment_line(&mut out, "", &format!("{name} — generated from a tree-sitter highlights.scm by `quin convert treesitter`."));
    comment_line(&mut out, "", "tree-sitter grammars are real parsers, so only the keyword lists and a few");
    comment_line(&mut out, "", "generic rules could be extracted. Treat this as a starting point and edit by hand.");
    out.push('\n');
    out.push_str(&format!("name {}\n", quote(&name)));
    out.push_str(&format!("extensions {}   # TODO: check the file extensions\n", quote(&name)));
    let c_style = facts.literals.contains("//") || facts.nodes.iter().any(|(node, _)| node == "line_comment" || node == "block_comment");
    out.push_str(if c_style { "line-comment //\n" } else { "line-comment #\n" });
    out.push('\n');

    // Keyword lists; a word that appears under several scopes keeps the first one.
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for (scope, words) in &facts.keywords {
        let words: Vec<String> = words.iter().filter(|w| seen.insert((*w).clone())).map(|w| quote(w)).collect();
        if !words.is_empty() {
            out.push_str(&format!("keywords {scope} {}\n", words.join(" ")));
        }
    }
    if !facts.operators.is_empty() {
        let mut ops: Vec<&String> = facts.operators.iter().collect();
        ops.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
        let alternatives: Vec<String> = ops.iter().map(|o| regex::escape(o)).collect();
        out.push_str(&format!("match operator {}\n", quote(&format!("(?:{})", alternatives.join("|")))));
    }
    out.push('\n');

    // Generic rules guessed from the node kinds the query mentions.
    if facts.has_node(&["comment"], "comment") {
        comment_line(&mut out, "", "TODO: adjust the comment syntax to the language");
        if c_style {
            out.push_str("match comment //.*\nregion comment /\\* \\*/\n");
        } else {
            out.push_str("match comment #.*\n");
        }
    }
    if facts.has_node(&["string", "template"], "string") {
        out.push_str("match string '\"(?:[^\"\\\\]|\\\\.)*\"'\nmatch string \"'(?:[^'\\\\]|\\\\.)*'\"\n");
    }
    if facts.nodes.iter().any(|(node, _)| ["number", "integer", "float"].iter().any(|n| node.contains(n))) {
        out.push_str("match number \\b\\d[\\d_]*(?:\\.\\d+)?(?:[eE][+-]?\\d+)?\\b\nmatch number \\b0[xX][0-9a-fA-F_]+\\b\n");
    }
    if facts.nodes.iter().any(|(_, s)| s == "function") {
        out.push_str("match function \\b([A-Za-z_]\\w*)\\s*\\( 1:function\n");
    }
    if facts.nodes.iter().any(|(_, s)| s.starts_with("type")) {
        out.push_str("match type \\b[A-Z][A-Za-z0-9_]*\\b\n");
    }
    if facts.has_node(&["attribute", "decorator", "annotation"], "attribute") {
        out.push_str("match attribute [@#]\\[?[A-Za-z_][\\w.]*\\]?\n");
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Command line
// ---------------------------------------------------------------------------

const USAGE: &str = "usage:\n  quin convert textmate <grammar.tmLanguage.json> [out.syn]\n  quin convert treesitter <language-name> <highlights.scm> [out.syn]";

/// `args` are the words after `convert`.
pub fn run_cli(args: &[String]) -> Result<(), String> {
    let (output, target) = match args {
        [kind, input, rest @ ..] if kind == "textmate" => {
            let source = std::fs::read_to_string(input).map_err(|e| format!("{input}: {e}"))?;
            (textmate_to_syn(&source)?, rest.first())
        }
        [kind, name, input, rest @ ..] if kind == "treesitter" => {
            let source = std::fs::read_to_string(input).map_err(|e| format!("{input}: {e}"))?;
            (treesitter_to_syn(name, &source)?, rest.first())
        }
        _ => return Err(USAGE.to_string()),
    };
    // Make sure what we produced is something quin itself can load.
    let mut scopes = super::Scopes::default();
    super::format::parse(&output, &mut scopes).map_err(|e| format!("internal error: generated file does not parse: {e}"))?;
    match target {
        Some(path) => std::fs::write(path, &output).map_err(|e| format!("{path}: {e}"))?,
        None => print!("{output}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::{Scopes, format};

    fn parses(syn: &str) -> crate::syntax::Language {
        let mut scopes = Scopes::default();
        format::parse(syn, &mut scopes).unwrap_or_else(|e| panic!("{e}\n---\n{syn}"))
    }

    #[test]
    fn rewrites_oniguruma_constructs() {
        assert_eq!(rewrite_regex(r"\h+(?=\s)x").unwrap(), "[0-9a-fA-F]+x");
        assert_eq!(rewrite_regex(r"(?<!\w)(foo)(?!\w)").unwrap(), r"\b(foo)\b");
        assert_eq!(rewrite_regex(r"(?>ab)c*+").unwrap(), "(?:ab)c*");
        assert_eq!(rewrite_regex(r"\Astart\z").unwrap(), "^start$");
        assert_eq!(rewrite_regex(r"a{,3}b{2}c{").unwrap(), r"a{0,3}b{2}c\{");
        assert_eq!(rewrite_regex(r"(?i:x)(?m)y\u00e9[\h]").unwrap(), r"(?i:x)(?s)y\x{00e9}[0-9a-fA-F]");
        assert_eq!(rewrite_regex(r"[[:alpha:]\]]").unwrap(), r"[[:alpha:]\]]");
        assert!(rewrite_regex(r"(a)\1").is_err());
        assert!(rewrite_regex(r"(?(1)a|b)").is_err());
        // Groups inside a dropped lookahead shift the numbering of later groups.
        let r = rewrite(r"a(?=(x))(y)(?<name>z)b").unwrap();
        assert_eq!(r.pattern, "a(y)(?<name>z)b");
        assert_eq!(r.groups, vec![None, Some(1), Some(2)]);
        assert_eq!(r.base_group, None);
        // Leading/trailing lookarounds become real context around a base group.
        let r = rewrite(r"(?<=\.)(\w+)(?=\()").unwrap();
        assert_eq!(r.pattern, r"(?:\.)((\w+))(?:\()");
        assert_eq!(r.groups, vec![Some(2)]);
        assert_eq!(r.base_group, Some(1));
        assert_eq!(rewrite_regex(r"(?<![\w.])(if|else)(?!\w)").unwrap(), r"\b(if|else)\b");
        assert_eq!(rewrite_regex(r"(?<!\w)\.foo(?!\w)").unwrap(), r"\.foo\b");
        assert_eq!(rewrite_regex(r"\<T\>").unwrap(), "<T>");
        assert_eq!(rewrite_regex(r"'|(?=\n)").unwrap(), "'|$");
        assert_eq!(rewrite_regex(r"(?x) (?<=a) b (?=c)").unwrap(), "(?x)(?:a)( b )(?:c)");
    }

    #[test]
    fn maps_scopes() {
        assert_eq!(map_textmate_scope("keyword.control.go"), "keyword.control");
        assert_eq!(map_textmate_scope("comment.block.documentation.rust"), "comment.doc");
        assert_eq!(map_textmate_scope("entity.name.function.member"), "function");
        assert_eq!(map_textmate_scope("string.quoted.double"), "string");
        assert_eq!(map_textmate_scope("weird.thing"), "special");
        assert_eq!(map_textmate_scope("punctuation.separator"), "punctuation");
    }

    #[test]
    fn quotes_arguments() {
        assert_eq!(quote(r"\d+"), r"\d+");
        assert_eq!(quote("a b"), "'a b'");
        assert_eq!(quote(r#""x""#), r#"'"x"'"#);
        assert_eq!(quote("it's"), "it's");
        assert_eq!(quote("'x'"), "\"'x'\"");
        assert_eq!(quote("' \""), "'\\x27 \"'");
    }

    const GRAMMAR: &str = r##"{
      "name": "Demo", "scopeName": "source.demo", "fileTypes": ["dm", ".demo"],
      "patterns": [
        {"include": "#comments"},
        {"include": "#keywords"},
        {"match": "\\b(fn)\\s+([a-z_]+)(?=\\()", "captures": {"1": {"name": "storage.type.demo"}, "2": {"name": "entity.name.function.demo"}}},
        {"match": "(a)\\1", "name": "invalid.illegal"},
        {"begin": "\"", "end": "\"", "name": "string.quoted.double.demo",
         "patterns": [{"match": "\\\\.", "name": "constant.character.escape.demo"}, {"include": "$self"}]},
        {"begin": "<<(\\w+)", "end": "^\\1$", "name": "string.heredoc"},
        {"include": "#self-ref"},
        {"match": "[0-9]+", "name": "constant.numeric.demo"}
      ],
      "repository": {
        "comments": {"patterns": [{"match": "//.*$", "name": "comment.line.double-slash.demo"},
                                  {"begin": "/\\*", "end": "\\*/", "name": "comment.block.demo"}]},
        "keywords": {"match": "\\b(if|else|while)\\b", "name": "keyword.control.demo"},
        "self-ref": {"patterns": [{"include": "#self-ref"}, {"match": "@\\w+", "name": "entity.other.attribute-name"}]}
      }
    }"##;

    #[test]
    fn converts_textmate_grammar() {
        let syn = textmate_to_syn(GRAMMAR).unwrap();
        let lang = parses(&syn);
        assert_eq!(lang.name, "demo");
        assert_eq!(lang.extensions, vec!["dm", "demo"]);
        assert!(syn.contains("keywords keyword.control if else while"));
        assert!(syn.contains("match comment //.*$"));
        assert!(syn.contains("match text (\\b(fn)\\s+([a-z_]+))(?:\\() 2:keyword.storage 3:function"));
        assert!(syn.contains("region string '\"' '\"' embed=demo\n  match string.escape"));
        assert!(syn.contains("# skipped (back-reference): (a)\\1"));
        assert!(syn.contains("# skipped (end pattern: back-reference)"));
        assert!(syn.contains("match attribute @\\w+"));
        assert!(syn.contains("match number [0-9]+"));
        // Two skipped rules, everything else converted into rules the parser accepted.
        assert_eq!(lang.rules.len(), 8);
    }

    const HIGHLIGHTS: &str = r#"
; comment
["if" "else" "while"] @keyword.conditional
"return" @keyword.return
["fn" "let"] @keyword
["true" "false"] @constant.builtin
["+" "-" "==" "->"] @operator
["(" ")"] @punctuation.bracket
(comment) @comment
(string_literal) @string
(integer_literal) @number
(call_expression function: (identifier) @function)
(type_identifier) @type
((identifier) @variable.builtin (#any-of? @variable.builtin "self" "super"))
"#;

    #[test]
    fn extracts_treesitter_keywords() {
        let syn = treesitter_to_syn("Demo", HIGHLIGHTS).unwrap();
        let lang = parses(&syn);
        assert_eq!(lang.name, "demo");
        assert!(syn.contains("keywords keyword.control else if return while"));
        assert!(syn.contains("keywords keyword fn let"));
        assert!(syn.contains("keywords constant.builtin false true"));
        assert!(syn.contains("keywords variable.builtin self super"));
        assert!(syn.contains("match operator (?:\\->|==|\\+|\\-)"));
        assert!(!syn.contains("punctuation"));
        assert!(syn.contains("match comment #.*"));
        assert!(syn.contains("match string"));
        assert!(syn.contains("match number"));
        assert!(syn.contains("1:function"));
        assert!(syn.contains("match type"));
    }
}
