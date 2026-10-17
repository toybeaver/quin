//! Links under the cursor (`g x`, Ctrl+click): Markdown links and images
//! (`[text](target)`, `![alt](target)`), `src=`/`href=` in HTML tags and bare
//! URLs. URLs open in the system's browser or mail program; paths open in
//! the editor when they hold text, in the system's viewer otherwise (images,
//! PDFs, directories).

use std::path::{Path, PathBuf};

use regex::Regex;

use crate::editor::Editor;

/// The link target around character `col` of `line`, if there is one.
pub fn link_at(line: &str, col: usize) -> Option<String> {
    let at = line.char_indices().nth(col).map(|(i, _)| i).unwrap_or(line.len());
    let covers = |start: usize, end: usize| start <= at && at < end.max(start + 1);
    // Markdown: anywhere on `[text](target "title")` or `![alt](<my target>)`.
    let markdown = Regex::new(r#"!?\[[^\]]*\]\(\s*(<[^>]*>|[^)\s]+)(?:\s+"[^"]*")?\s*\)"#).expect("valid regex");
    for found in markdown.captures_iter(line) {
        let whole = found.get(0)?;
        if covers(whole.start(), whole.end()) {
            let target = found.get(1)?.as_str();
            return Some(target.trim_start_matches('<').trim_end_matches('>').to_string());
        }
    }
    // HTML: anywhere on a tag with a `src` or `href`.
    let tag = Regex::new(r"<[^<>]*>").expect("valid regex");
    let attribute = Regex::new(r#"(?i)\b(?:src|href)\s*=\s*(?:"([^"]*)"|'([^']*)')"#).expect("valid regex");
    for found in tag.find_iter(line) {
        if covers(found.start(), found.end())
            && let Some(value) = attribute.captures(found.as_str())
        {
            return Some(value.get(1).or(value.get(2))?.as_str().to_string());
        }
    }
    // A bare URL, without the punctuation that usually ends a sentence.
    let url = Regex::new(r#"(?:https?|ftp|file|mailto):[^\s<>"'`]+"#).expect("valid regex");
    for found in url.find_iter(line) {
        if covers(found.start(), found.end()) {
            let mut url = found.as_str().trim_end_matches(['.', ',', ';', ':', '!', '?']);
            // A closing bracket belongs to the URL only when it opened inside it.
            for (open, close) in [('(', ')'), ('[', ']')] {
                while url.ends_with(close) && url.matches(open).count() < url.matches(close).count() {
                    url = &url[..url.len() - 1];
                }
            }
            return Some(url.to_string());
        }
    }
    None
}

/// Follow a link: a URL goes to the system, a path is opened (relative to
/// the buffer's directory).
pub fn follow(ed: &mut Editor, target: &str) {
    let target = target.trim();
    if has_scheme(target) {
        return launch(ed, target);
    }
    // `other.md#section` and `page.html?x=1` name the file before `#` and `?`.
    let path_text = target.split(['#', '?']).next().unwrap_or("");
    if path_text.is_empty() {
        return ed.set_error("links inside the file are not followed");
    }
    let path = crate::files::expand_home(&crate::clipboard::percent_decode(path_text));
    let base = ed.buffer().path.as_ref().and_then(|p| p.parent()).map(Path::to_path_buf).unwrap_or_else(|| ed.cwd.clone());
    let path = if path.is_absolute() { path } else { base.join(path) };
    if !path.exists() {
        return ed.set_error(format!("not found: {}", path.display()));
    }
    if path.is_dir() || !holds_text(&path) {
        return launch(ed, &path.to_string_lossy());
    }
    crate::overlays::open_path(ed, &path, None, crate::picker::OpenMode::Here);
}

/// `https:`, `mailto:`... but not a Windows drive (`C:\`).
fn has_scheme(target: &str) -> bool {
    match target.split_once(':') {
        Some((scheme, _)) => {
            scheme.len() > 1
                && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
                && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
        }
        None => false,
    }
}

/// Text files have no NUL byte in their first few kilobytes.
fn holds_text(path: &PathBuf) -> bool {
    let mut head = [0u8; 8192];
    let read = std::fs::File::open(path).and_then(|mut file| std::io::Read::read(&mut file, &mut head));
    read.is_ok_and(|n| !head[..n].contains(&0))
}

/// Hand a URL or a path to the system. Tests only record it.
fn launch(ed: &mut Editor, target: &str) {
    if let Some(launched) = &mut ed.launched {
        launched.push(target.to_string());
        return;
    }
    match crate::files::open_with_system(target) {
        Ok(()) => ed.set_message(format!("opening {target}")),
        Err(err) => ed.set_error(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_markdown_html_and_bare_links() {
        let line = "see [the docs](docs/a%20b.md#intro \"Title\") and ![logo](<my logo.png>)";
        assert_eq!(link_at(line, 5), Some("docs/a%20b.md#intro".to_string()), "on the text");
        assert_eq!(link_at(line, 20), Some("docs/a%20b.md#intro".to_string()), "on the target");
        assert_eq!(link_at(line, 60), Some("my logo.png".to_string()));
        assert_eq!(link_at(line, 1), None, "outside any link");
        let html = r#"<img src="icon.png" width="160"> <a href='https://x.org/'>x</a>"#;
        assert_eq!(link_at(html, 25), Some("icon.png".to_string()));
        assert_eq!(link_at(html, 40), Some("https://x.org/".to_string()));
        let bare = "Visit https://example.org/path_(x). Or (https://a.b/c).";
        assert_eq!(link_at(bare, 10), Some("https://example.org/path_(x)".to_string()));
        assert_eq!(link_at(bare, 45), Some("https://a.b/c".to_string()));
        assert!(has_scheme("mailto:me@x.org") && !has_scheme("C:\\file.txt") && !has_scheme("docs/a.md"));
    }
}
