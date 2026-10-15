//! Parser for quin's syntax definition format.
//!
//! The format is line oriented. Arguments are separated by whitespace; an argument
//! that contains spaces is written between single or double quotes (there are no
//! escape sequences, pick the quote that does not appear inside). Lines starting
//! with `#` are comments.
//!
//! ```text
//! name          rust                  # language name (lower case)
//! extensions    rs                    # file extensions, without the dot
//! filenames     Makefile GNUmakefile  # exact file names
//! shebang       bash sh               # words looked up in a `#!` first line
//! line-comment  //                    # token used by "toggle comments"
//! block-comment /* */
//! indent        4 spaces              # or: `indent tabs`
//! auto-pairs    () [] {} "" ''
//!
//! keywords <scope> word word ...      # words highlighted with <scope>
//! match    <scope> <regex> [N:<scope> ...]
//!                                     # one-line pattern; captures may get their own scope
//! region   <scope> <start-regex> <end-regex> [embed=<lang>|embed=N] [N:<scope> ...]
//!   match  <scope> <regex>            # indented rules apply inside the region
//!
//! style    <scope> [bold] [italic] [underline] [strike] [conceal]
//!                  [size=1.4] [bg=<scope>] [replace=<text>]
//! ```
//!
//! Rules are tried in order at every position; the earliest match wins, ties go
//! to the rule listed first. Regexes use Rust's `regex` syntax (no lookaround or
//! back-references). `^` and `$` match the start and end of the line.

use regex::Regex;

use super::{Embed, IndentStyle, Language, Rule, ScopeId, Scopes, StyleAttrs};

pub fn parse(source: &str, scopes: &mut Scopes) -> Result<Language, String> {
    let mut lang = Language::default();
    // Index of the most recent top-level region, so indented rules can attach to it.
    let mut open_region: Option<usize> = None;

    for (line_no, raw) in source.lines().enumerate() {
        let line_no = line_no + 1;
        let indented = raw.starts_with(' ') || raw.starts_with('\t');
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let args = tokenize(line);
        let directive = args[0].as_str();
        let rest = &args[1..];
        let err = |msg: &str| format!("line {line_no}: {msg}");

        match directive {
            "name" => lang.name = rest.first().ok_or_else(|| err("name needs a value"))?.to_ascii_lowercase(),
            "extensions" | "extension" => lang.extensions.extend(rest.iter().map(|e| e.to_ascii_lowercase())),
            "filenames" | "filename" => lang.filenames.extend(rest.iter().map(|e| e.to_ascii_lowercase())),
            "shebang" | "shebangs" => lang.shebangs.extend(rest.iter().cloned()),
            "line-comment" => lang.line_comment = rest.first().cloned(),
            "block-comment" => {
                if rest.len() != 2 {
                    return Err(err("block-comment needs a start and an end token"));
                }
                lang.block_comment = Some((rest[0].clone(), rest[1].clone()));
            }
            "indent" => lang.indent = Some(parse_indent(rest).ok_or_else(|| err("bad indent, use `indent 4 spaces` or `indent tabs`"))?),
            "auto-pairs" => {
                lang.auto_pairs = rest
                    .iter()
                    .filter_map(|pair| {
                        let mut chars = pair.chars();
                        Some((chars.next()?, chars.next()?))
                    })
                    .collect();
            }
            "keywords" => {
                if rest.len() < 2 {
                    return Err(err("keywords needs a scope and at least one word"));
                }
                let regex = keyword_regex(&rest[1..]);
                let rule = Rule::Match { scope: scopes.intern(&rest[0]), regex, captures: Vec::new() };
                push_rule(&mut lang, rule, indented, open_region);
            }
            "match" => {
                if rest.len() < 2 {
                    return Err(err("match needs a scope and a regex"));
                }
                let regex = compile(&rest[1]).map_err(|e| err(&e))?;
                let captures = parse_captures(&rest[2..], scopes).map_err(|e| err(&e))?;
                let rule = Rule::Match { scope: scopes.intern(&rest[0]), regex, captures };
                push_rule(&mut lang, rule, indented, open_region);
            }
            "region" => {
                if rest.len() < 3 {
                    return Err(err("region needs a scope, a start regex and an end regex"));
                }
                let start = compile(&rest[1]).map_err(|e| err(&e))?;
                let end = compile(&rest[2]).map_err(|e| err(&e))?;
                let mut embed = None;
                let mut capture_args = Vec::new();
                for arg in &rest[3..] {
                    if let Some(value) = arg.strip_prefix("embed=") {
                        embed = Some(match value.parse::<usize>() {
                            Ok(group) => Embed::Capture(group),
                            Err(_) => Embed::Language(value.to_ascii_lowercase()),
                        });
                    } else {
                        capture_args.push(arg.clone());
                    }
                }
                let start_captures = parse_captures(&capture_args, scopes).map_err(|e| err(&e))?;
                let rule = Rule::Region { scope: scopes.intern(&rest[0]), start, end, start_captures, inner: Vec::new(), embed };
                let index = push_rule(&mut lang, rule, indented, open_region);
                if !indented {
                    open_region = Some(index);
                }
                continue;
            }
            "style" => {
                if rest.is_empty() {
                    return Err(err("style needs a scope"));
                }
                let attrs = parse_style(&rest[1..]).map_err(|e| err(&e))?;
                lang.styles.push((scopes.intern(&rest[0]), attrs));
            }
            other => return Err(err(&format!("unknown directive `{other}`"))),
        }
        if !indented {
            open_region = None;
        }
    }

    if lang.name.is_empty() {
        return Err("missing `name` directive".to_string());
    }
    Ok(lang)
}

/// Append a rule to the language, either at the top level or inside the region
/// that an indented line belongs to. Returns the rule's index.
fn push_rule(lang: &mut Language, rule: Rule, indented: bool, open_region: Option<usize>) -> usize {
    let index = lang.rules.len();
    lang.rules.push(rule);
    match (indented, open_region) {
        (true, Some(region)) => {
            if let Rule::Region { inner, .. } = &mut lang.rules[region] {
                inner.push(index);
            }
        }
        _ => lang.top.push(index),
    }
    index
}

fn compile(pattern: &str) -> Result<Regex, String> {
    Regex::new(pattern).map_err(|e| format!("bad regex `{pattern}`: {e}"))
}

/// One regex matching any of the words, with word boundaries where the word
/// starts/ends with a word character (so `#include` or `@media` work too).
pub fn keyword_regex(words: &[String]) -> Regex {
    let mut words: Vec<&String> = words.iter().collect();
    // Longest first so `elif` is not matched as `el` + `if` by a prefix.
    words.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
    let alternatives: Vec<String> = words
        .iter()
        .map(|w| {
            let first = w.chars().next().is_some_and(is_word_char);
            let last = w.chars().last().is_some_and(is_word_char);
            format!("{}{}{}", if first { r"\b" } else { "" }, regex::escape(w), if last { r"\b" } else { "" })
        })
        .collect();
    Regex::new(&format!("(?:{})", alternatives.join("|"))).expect("escaped keywords always compile")
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn parse_captures(args: &[String], scopes: &mut Scopes) -> Result<Vec<(usize, ScopeId)>, String> {
    let mut captures = Vec::new();
    for arg in args {
        let (group, scope) = arg.split_once(':').ok_or_else(|| format!("expected N:scope, got `{arg}`"))?;
        let group: usize = group.parse().map_err(|_| format!("bad capture group in `{arg}`"))?;
        captures.push((group, scopes.intern(scope)));
    }
    Ok(captures)
}

fn parse_indent(args: &[String]) -> Option<IndentStyle> {
    match args {
        [kind] if kind == "tabs" => Some(IndentStyle { width: 4, use_tabs: true }),
        [width] => Some(IndentStyle { width: width.parse().ok()?, use_tabs: false }),
        [width, kind] => Some(IndentStyle { width: width.parse().ok()?, use_tabs: kind == "tabs" }),
        _ => None,
    }
}

fn parse_style(args: &[String]) -> Result<StyleAttrs, String> {
    let mut attrs = StyleAttrs::default();
    for arg in args {
        match arg.as_str() {
            "bold" => attrs.bold = true,
            "italic" => attrs.italic = true,
            "underline" => attrs.underline = true,
            "strike" => attrs.strike = true,
            "conceal" => attrs.conceal = true,
            _ => match arg.split_once('=') {
                Some(("size", v)) => attrs.size = Some(v.parse().map_err(|_| format!("bad size `{v}`"))?),
                Some(("bg", v)) => attrs.bg = Some(v.to_string()),
                Some(("replace", v)) => attrs.replace = Some(v.to_string()),
                _ => return Err(format!("unknown style attribute `{arg}`")),
            },
        }
    }
    Ok(attrs)
}

/// Split a line into arguments. Quoted arguments keep their spaces verbatim.
pub fn tokenize(line: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        let mut arg = String::new();
        if c == '"' || c == '\'' {
            chars.next();
            for ch in chars.by_ref() {
                if ch == c {
                    break;
                }
                arg.push(ch);
            }
        } else {
            while let Some(&ch) = chars.peek() {
                if ch.is_whitespace() {
                    break;
                }
                arg.push(ch);
                chars.next();
            }
        }
        args.push(arg);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_quotes() {
        assert_eq!(tokenize(r#"match string '"[^"]*"' 1:x"#), vec!["match", "string", r#""[^"]*""#, "1:x"]);
        assert_eq!(tokenize("a  'b c'   d"), vec!["a", "b c", "d"]);
    }

    #[test]
    fn parses_small_language() {
        let source = "
name demo
extensions dm
line-comment //
keywords keyword if else
match number \\d+
region comment /\\* \\*/
  match todo TODO
style heading bold size=1.5
";
        let mut scopes = Scopes::default();
        let lang = parse(source, &mut scopes).unwrap();
        assert_eq!(lang.name, "demo");
        assert_eq!(lang.top.len(), 3);
        assert_eq!(lang.rules.len(), 4);
        match &lang.rules[2] {
            Rule::Region { inner, .. } => assert_eq!(inner, &vec![3]),
            _ => panic!("expected region"),
        }
        assert_eq!(lang.styles[0].1.size, Some(1.5));
    }
}
