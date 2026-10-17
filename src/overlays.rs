//! Opening and driving the prompt and picker overlays: search, select-regex,
//! file/buffer pickers, the command palette, global search.

use std::path::{Path, PathBuf};

use crate::buffer::BufferId;
use crate::commands;
use crate::editor::LiveSearch;
use crate::editor::{Editor, Mode, Overlay};
use crate::frame::Dir;
use crate::keymap::Key;
use crate::picker::{ItemData, OpenMode, Picker, PickerEvent, PickerItem, PickerKind, Prompt, PromptEvent, PromptKind};
use crate::selection::{Range, Selection};
use crate::typed;

// ---------------------------------------------------------------------------
// Prompt
// ---------------------------------------------------------------------------

pub fn open_prompt(ed: &mut Editor, kind: PromptKind, label: &str) {
    let origin = ed.view().selection.clone();
    let scroll = ed.view().scroll_line;
    ed.overlay = Overlay::Prompt(Prompt::new(kind, label, origin, scroll));
}

/// `C-S-v` in a prompt or picker: the clipboard's first line, if any.
fn pasted_line(ed: &mut Editor, key: Key) -> Option<String> {
    if key != Key::parse("C-S-v").expect("valid key") {
        return None;
    }
    let text = ed.clipboard.paste().ok()?;
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_string())
}

pub fn prompt_key(ed: &mut Editor, key: Key) {
    let pasted = pasted_line(ed, key);
    let Overlay::Prompt(prompt) = &mut ed.overlay else { return };
    let event = match pasted {
        Some(text) => {
            prompt.insert(&text);
            PromptEvent::Changed
        }
        None => prompt.handle_key(key),
    };
    match event {
        PromptEvent::Pending => {}
        PromptEvent::Changed => {
            if let PromptKind::Search { reverse, extend } = prompt.kind {
                let origin = prompt.origin.clone();
                let text = prompt.text.clone();
                search_preview(ed, &text, origin, reverse, extend);
            }
        }
        PromptEvent::History(delta) => {
            if matches!(prompt.kind, PromptKind::Search { .. } | PromptKind::SelectRegex) {
                let history = &ed.search.history;
                if history.is_empty() {
                    return;
                }
                let index = match prompt.history_index {
                    None if delta < 0 => history.len() - 1,
                    None => return,
                    Some(i) => (i as isize + delta).clamp(0, history.len() as isize - 1) as usize,
                };
                prompt.history_index = Some(index);
                let text = history[index].clone();
                prompt.set_text(&text);
                if let PromptKind::Search { reverse, extend } = prompt.kind {
                    let origin = prompt.origin.clone();
                    search_preview(ed, &text, origin, reverse, extend);
                }
            }
        }
        PromptEvent::Cancel => {
            let prompt = std::mem::replace(&mut ed.overlay, Overlay::None);
            if let Overlay::Prompt(prompt) = prompt
                && matches!(prompt.kind, PromptKind::Search { .. })
            {
                let len = ed.buffer().len_chars();
                let view = ed.view_mut();
                view.selection = prompt.origin.clamp(len);
                view.scroll_line = prompt.origin_scroll;
            }
        }
        PromptEvent::Submit(text) => {
            let prompt = std::mem::replace(&mut ed.overlay, Overlay::None);
            if let Overlay::Prompt(prompt) = prompt {
                prompt_submit(ed, prompt, text);
            }
        }
    }
}

fn remember_search(ed: &mut Editor, text: &str) {
    if !text.is_empty() {
        ed.search.history.retain(|h| h != text);
        ed.search.history.push(text.to_string());
    }
}

fn prompt_submit(ed: &mut Editor, prompt: Prompt, text: String) {
    match prompt.kind {
        PromptKind::Search { reverse, extend } => {
            let pattern = if text.is_empty() { ed.search.history.last().cloned().unwrap_or_default() } else { text };
            remember_search(ed, &pattern);
            match crate::search::compile(&pattern) {
                Ok(regex) => {
                    ed.search.regex = Some(regex);
                    let len = ed.buffer().len_chars();
                    ed.view_mut().selection = prompt.origin.clamp(len);
                    ed.view_mut().save_jump();
                    search_step(ed, reverse, extend, true);
                }
                Err(err) => ed.set_error(format!("bad regex: {err}")),
            }
        }
        PromptKind::SelectRegex => with_regex(ed, &text, select_on_regex),
    }
}

fn with_regex(ed: &mut Editor, text: &str, f: impl FnOnce(&mut Editor, &regex::Regex)) {
    if text.is_empty() {
        return;
    }
    remember_search(ed, text);
    match crate::search::compile(text) {
        Ok(regex) => f(ed, &regex),
        Err(err) => ed.set_error(format!("bad regex: {err}")),
    }
}

/// Incremental search: show the match that `Enter` would select.
fn search_preview(ed: &mut Editor, pattern: &str, origin: Selection, reverse: bool, extend: bool) {
    let len = ed.buffer().len_chars();
    ed.view_mut().selection = origin.clamp(len);
    if pattern.is_empty() {
        return;
    }
    if let Ok(regex) = crate::search::compile(pattern) {
        let found = move_to_match(ed, &regex, reverse, extend);
        if found {
            ed.ensure_cursor_visible();
        }
    }
}

/// Move the primary selection to the next/previous match. Returns false if none.
fn move_to_match(ed: &mut Editor, regex: &regex::Regex, reverse: bool, extend: bool) -> bool {
    let (view, buffer) = ed.current();
    let primary = view.selection.primary();
    let from = if reverse { primary.from() } else { primary.head.min(buffer.len_chars()) + 1 };
    match crate::search::find(&buffer.text, regex, from.min(buffer.len_chars()), reverse) {
        Some((start, end)) => {
            let range = if extend {
                Range::new(primary.anchor, end.saturating_sub(1).max(start))
            } else {
                Range::new(start, end.saturating_sub(1).max(start))
            };
            let mut ranges: Vec<Range> = view.selection.ranges().to_vec();
            let index = view.selection.primary_index();
            ranges[index] = range;
            view.selection = Selection::from_ranges(ranges, index);
            true
        }
        None => false,
    }
}

/// `n` / `N`: jump to the next or previous match of the current pattern.
pub fn search_step(ed: &mut Editor, reverse: bool, extend: bool, report: bool) {
    let Some(regex) = ed.search.regex.clone() else {
        ed.set_error("no search pattern (use / first)");
        return;
    };
    if !move_to_match(ed, &regex, reverse, extend) && report {
        ed.set_error(format!("no match for {}", regex.as_str()));
    }
}

fn select_on_regex(ed: &mut Editor, regex: &regex::Regex) {
    let (view, buffer) = ed.current();
    let mut ranges = Vec::new();
    for r in view.selection.iter() {
        let (from, to) = r.span(&buffer.text);
        for (start, end) in crate::search::find_all(&buffer.text, regex, from, to) {
            ranges.push(Range::new(start, end - 1));
        }
    }
    if ranges.is_empty() {
        ed.set_error("no matches");
        return;
    }
    let primary = ranges.len() - 1;
    view.selection = Selection::from_ranges(ranges, primary);
}

// ---------------------------------------------------------------------------
// Pickers
// ---------------------------------------------------------------------------

/// Most hits a global search collects.
const SEARCH_LIMIT: usize = 2000;

/// Pick a file. The list starts with every file below the working directory
/// (`all` includes ignored and hidden ones); typing a path (`/`, `~/`, `./`,
/// `../`) switches to browsing that directory, any directory on the machine.
pub fn open_file_picker(ed: &mut Editor, all: bool) {
    let items = recursive_file_items(ed, all);
    let title = if all { "all files" } else { "files" };
    ed.overlay = Overlay::Picker(Picker::new(PickerKind::Files, title, items));
}

fn recursive_file_items(ed: &mut Editor, all: bool) -> Vec<PickerItem> {
    let root = ed.cwd.clone();
    if ed.file_list.as_ref().is_none_or(|(cached_all, _)| *cached_all != all) {
        ed.file_list = Some((all, crate::files::list_files(&root, crate::files::FILE_LIMIT, all)));
    }
    let files = ed.file_list.as_ref().map(|(_, files)| files.clone()).unwrap_or_default();
    files.into_iter().map(|rel| PickerItem::new(rel.to_string_lossy().replace('\\', "/"), "", ItemData::Path(root.join(rel)))).collect()
}

/// The directory part of a query that browses the file system (`~/src/` of
/// `~/src/ma`), or `None` for a plain fuzzy query over the working directory.
fn browse_prefix(query: &str) -> Option<String> {
    let browsing = query.starts_with('/') || query.starts_with('~') || query.starts_with("./") || query.starts_with("../");
    let slash = query.rfind('/')?;
    browsing.then(|| query[..=slash].to_string())
}

fn expand_path(text: &str) -> PathBuf {
    crate::files::expand_home(text)
}

/// Keep the file picker's items in step with its query: list the directory
/// being browsed, or go back to the recursive list.
fn sync_file_picker(ed: &mut Editor) {
    let Overlay::Picker(picker) = &ed.overlay else { return };
    if picker.kind != PickerKind::Files {
        return;
    }
    let prefix = browse_prefix(&picker.query);
    if prefix == picker.browse {
        return;
    }
    let all = picker.title == "all files";
    let items = match &prefix {
        Some(dir_text) => {
            let dir = expand_path(dir_text);
            let mut items = vec![PickerItem::new("../", "", ItemData::Dir)];
            items.extend(crate::files::list_dir(&dir).into_iter().map(|(name, is_dir)| {
                let data = if is_dir { ItemData::Dir } else { ItemData::Path(dir.join(&name)) };
                PickerItem::new(name, "", data)
            }));
            items
        }
        None => recursive_file_items(ed, all),
    };
    let Overlay::Picker(picker) = &mut ed.overlay else { return };
    picker.needle_start = prefix.as_ref().map(|p| p.len()).unwrap_or(0);
    picker.browse = prefix;
    picker.selected = 0;
    picker.set_items(items);
}

pub fn open_buffer_picker(ed: &mut Editor) {
    let mut items = Vec::new();
    for id in ed.buffer_ids() {
        let buffer = &ed.buffers[&id];
        let flags = if buffer.is_modified() { " [+]" } else { "" };
        let language = buffer.language.map(|l| ed.registry.get(l).name.clone()).unwrap_or_default();
        items.push(PickerItem::new(format!("{}{flags}", buffer.display_name()), language, ItemData::Buffer(id)));
    }
    ed.overlay = Overlay::Picker(Picker::new(PickerKind::Buffers, "buffers", items));
}

pub fn open_command_palette(ed: &mut Editor) {
    let items = command_items(ed);
    ed.overlay = Overlay::Picker(Picker::new(PickerKind::Commands, "command", items));
}

/// The palette with `query` already typed (`:sh` without a command opens it on `sh `).
pub fn open_command_palette_with(ed: &mut Editor, query: &str) {
    open_command_palette(ed);
    if let Overlay::Picker(picker) = &mut ed.overlay {
        picker.query = query.to_string();
        picker.filter();
    }
    sync_command_arguments(ed);
}

/// Every typed command and key command, for the palette.
fn command_items(ed: &Editor) -> Vec<PickerItem> {
    let bindings = ed.keymaps.normal.bindings();
    let mut items = Vec::new();
    for cmd in typed::COMMANDS {
        let mut detail = cmd.doc.to_string();
        if !cmd.aliases.is_empty() {
            detail = format!("{detail}  (:{})", cmd.aliases.join(", :"));
        }
        items.push(PickerItem::new(cmd.name, detail, ItemData::Typed(cmd)));
    }
    let mut key_commands: Vec<PickerItem> = commands::COMMANDS
        .iter()
        .map(|cmd| {
            let mut keys: Vec<&str> = bindings.iter().filter(|(_, c)| c.name == cmd.name).map(|(k, _)| k.as_str()).collect();
            keys.sort_by_key(|k| k.len());
            let detail = if keys.is_empty() { cmd.doc.to_string() } else { format!("{}  [{}]", cmd.doc, keys.join(", ")) };
            PickerItem::new(cmd.name, detail, ItemData::Command(cmd))
        })
        .collect();
    key_commands.sort_by(|a, b| a.label.cmp(&b.label));
    items.extend(key_commands);
    items
}

/// Palette commands whose argument is a path, and whether only directories fit.
const PATH_COMMANDS: &[(&str, bool)] = &[("cd", true), ("open", false), ("write", false)];

/// The shell command typed in the palette (`!ls -l`, `sh ls -l`), with the
/// byte offset where it starts in the query.
fn shell_line(query: &str) -> Option<(usize, &str)> {
    if let Some(rest) = query.strip_prefix('!') {
        return Some((1, rest));
    }
    let (name, rest) = query.split_once(' ')?;
    (typed::find(name)?.name == "sh").then_some((name.len() + 1, rest))
}

/// While a shell command is typed, the palette completes its last word:
/// program names for the first word, paths for the others.
fn sync_shell_completion(ed: &mut Editor, line_start: usize, line: &str) {
    let word_start = line.rfind(char::is_whitespace).map(|i| i + 1).unwrap_or(0);
    let word = &line[word_start..];
    let start = line_start + word_start;
    let program = line[..word_start].trim().is_empty() && !word.contains('/');
    let dir_text = if program { String::new() } else { word.rfind('/').map(|i| word[..=i].to_string()).unwrap_or_default() };
    let key = if program { format!("sh {start}") } else { format!("sh {start}:{dir_text}") };
    let Overlay::Picker(picker) = &ed.overlay else { return };
    if picker.browse.as_deref() == Some(key.as_str()) {
        return;
    }
    let items: Vec<PickerItem> = if program {
        crate::files::list_executables().into_iter().map(|name| PickerItem::new(name.clone(), "", ItemData::Argument(name))).collect()
    } else {
        let dir = if dir_text.is_empty() { ed.cwd.clone() } else { ed.cwd.join(expand_path(&dir_text)) };
        crate::files::list_dir(&dir)
            .into_iter()
            .map(|(name, _)| PickerItem::new(name.clone(), "", ItemData::Argument(format!("{dir_text}{name}"))))
            .collect()
    };
    let Overlay::Picker(picker) = &mut ed.overlay else { return };
    picker.browse = Some(key);
    picker.needle_start = start + dir_text.len();
    picker.selected = 0;
    picker.set_items(items);
}

/// While the argument of `cd` or `open` is typed, the palette lists the
/// entries of the directory typed so far instead of the commands; a shell
/// command (`!`, `sh`) gets its own completion.
fn sync_command_arguments(ed: &mut Editor) {
    let Overlay::Picker(picker) = &ed.overlay else { return };
    if let Some((line_start, line)) = shell_line(&picker.query) {
        let line = line.to_string();
        sync_shell_completion(ed, line_start, &line);
        return;
    }
    let argument = picker.query.split_once(' ').and_then(|(name, rest)| {
        let command = typed::find(name)?;
        let (_, dirs_only) = PATH_COMMANDS.iter().find(|(c, _)| *c == command.name)?;
        Some((name.len() + 1, rest.trim_start().to_string(), *dirs_only))
    });
    let Some((prefix_len, argument, dirs_only)) = argument else {
        if picker.browse.is_some() {
            let items = command_items(ed);
            let Overlay::Picker(picker) = &mut ed.overlay else { return };
            picker.browse = None;
            picker.needle_start = 0;
            picker.set_items(items);
        }
        return;
    };
    let dir_text = argument.rfind('/').map(|i| argument[..=i].to_string()).unwrap_or_default();
    let key = format!("{prefix_len}:{dir_text}");
    if picker.browse.as_deref() == Some(key.as_str()) {
        return;
    }
    let dir = if dir_text.is_empty() { ed.cwd.clone() } else { expand_path(&dir_text) };
    // `cd` lists the directory itself first, so `ret` on an empty argument
    // stays here instead of going into the first entry (often `.git/`).
    let here = dirs_only.then(|| PickerItem::new("./", "", ItemData::Argument(format!("{dir_text}."))));
    let entries = crate::files::list_dir(&dir)
        .into_iter()
        .filter(|(_, is_dir)| *is_dir || !dirs_only)
        .map(|(name, _)| PickerItem::new(name.clone(), "", ItemData::Argument(format!("{dir_text}{name}"))));
    let items: Vec<PickerItem> = here.into_iter().chain(entries).collect();
    let Overlay::Picker(picker) = &mut ed.overlay else { return };
    picker.browse = Some(key);
    picker.needle_start = prefix_len + dir_text.len();
    picker.selected = 0;
    picker.set_items(items);
}

pub fn open_theme_picker(ed: &mut Editor) {
    let mut names: Vec<String> = crate::theme::Theme::builtin_names().iter().map(|s| s.to_string()).collect();
    if let Some(dir) = crate::config::config_dir()
        && let Ok(entries) = std::fs::read_dir(dir.join("themes"))
    {
        for entry in entries.flatten() {
            if let Some(name) = entry.path().file_stem().and_then(|s| s.to_str())
                && entry.path().extension().and_then(|e| e.to_str()) == Some("theme")
                && !names.contains(&name.to_string())
            {
                names.push(name.to_string());
            }
        }
    }
    let items = names.into_iter().map(|n| PickerItem::new(n.clone(), "", ItemData::Theme(n))).collect();
    ed.theme_before_picker = Some(ed.theme.clone());
    ed.overlay = Overlay::Picker(Picker::new(PickerKind::Themes, "theme", items));
}

/// Show the highlighted theme on the whole window while the picker is open.
fn preview_theme(ed: &mut Editor) {
    let Overlay::Picker(picker) = &ed.overlay else { return };
    let Some(ItemData::Theme(name)) = picker.selected_item().map(|i| &i.data) else { return };
    if let Some(theme) = typed::load_theme(name) {
        ed.set_theme(theme);
    }
}

/// The picker closed without choosing: back to the theme in use before.
fn restore_theme(ed: &mut Editor) {
    if let Some(theme) = ed.theme_before_picker.take() {
        ed.set_theme(theme);
    }
}

pub fn open_language_picker(ed: &mut Editor) {
    let mut names: Vec<String> = ed.registry.language_names().iter().map(|s| s.to_string()).collect();
    names.sort();
    let mut items = vec![PickerItem::new("plain text", "", ItemData::Language(String::new()))];
    items.extend(names.into_iter().map(|n| PickerItem::new(n.clone(), "", ItemData::Language(n))));
    ed.overlay = Overlay::Picker(Picker::new(PickerKind::Languages, "language", items));
}

/// Global search: a results picker whose query is the regex; the search runs
/// in the background and the list fills in as hits arrive.
pub fn open_global_search(ed: &mut Editor) {
    ed.overlay = Overlay::Picker(Picker::new(PickerKind::Locations, "global search", Vec::new()));
    // A search pattern set with `*` (or `/`): look for it everywhere right away.
    let pattern = ed.search.history.last().cloned().filter(|p| !p.trim().is_empty());
    if let (Some(pattern), Overlay::Picker(picker)) = (pattern, &mut ed.overlay) {
        picker.query = pattern;
        restart_global_search(ed);
    }
}

/// (Re)start the background search for the picker's current query.
fn restart_global_search(ed: &mut Editor) {
    ed.live_search = None; // cancels the previous one
    let Overlay::Picker(picker) = &mut ed.overlay else { return };
    picker.set_items(Vec::new());
    let query = picker.query.clone();
    if query.is_empty() {
        return;
    }
    let Ok(regex) = crate::search::compile(&query) else { return };
    let root = ed.cwd.clone();
    if ed.file_list.as_ref().is_none_or(|(all, _)| *all) {
        ed.file_list = Some((false, crate::files::list_files(&root, crate::files::FILE_LIMIT, false)));
    }
    let files = ed.file_list.as_ref().map(|(_, files)| files.clone()).unwrap_or_default();
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let wake = ed.wake.clone().unwrap_or_else(|| std::sync::Arc::new(|| {}));
    let results = crate::files::grep_in_background(root, files, regex, SEARCH_LIMIT, cancel.clone(), wake);
    ed.live_search = Some(LiveSearch { results, cancel, done: false });
}

/// Append hits from the background search to the results picker.
pub fn add_search_results(ed: &mut Editor, hits: Vec<crate::files::GrepHit>) {
    let root = ed.cwd.clone();
    let Overlay::Picker(picker) = &mut ed.overlay else { return };
    if picker.kind != PickerKind::Locations {
        return;
    }
    for hit in hits {
        let path = hit.path.to_string_lossy().replace('\\', "/");
        let prefix = format!("{path}:{}: ", hit.line + 1);
        let prefix_len = prefix.chars().count();
        let mut item =
            PickerItem::new(format!("{prefix}{}", hit.text), "", ItemData::Location { path: root.join(hit.path), line: hit.line });
        item.highlight = (prefix_len + hit.matched.0..prefix_len + hit.matched.1).collect();
        picker.items.push(item);
    }
    picker.filter();
}

pub fn picker_key(ed: &mut Editor, key: Key) {
    let pasted = pasted_line(ed, key);
    let Overlay::Picker(picker) = &mut ed.overlay else { return };
    let kind = picker.kind;
    let query_before = picker.query.clone();
    let event = match pasted {
        Some(text) => {
            picker.query.push_str(&text);
            picker.filter();
            PickerEvent::Pending
        }
        None => picker.handle_key(key),
    };
    match event {
        PickerEvent::Pending => match kind {
            PickerKind::Files => sync_file_picker(ed),
            PickerKind::Commands => sync_command_arguments(ed),
            PickerKind::Locations if picker.query != query_before => restart_global_search(ed),
            PickerKind::Themes => preview_theme(ed),
            _ => {}
        },
        PickerEvent::Close => {
            close_picker(ed);
            restore_theme(ed);
        }
        PickerEvent::Submit(query) => {
            close_picker(ed);
            restore_theme(ed);
            match kind {
                PickerKind::Commands => run_palette_text(ed, &query),
                // A path that is not listed: open (or create) that file.
                PickerKind::Files if !query.trim().is_empty() => open_path(ed, &expand_path(query.trim()), None, OpenMode::Here),
                _ => {}
            }
        }
        PickerEvent::Choose { item, query, open } => {
            // A shell command runs as typed; `tab` completes the highlighted word.
            if kind == PickerKind::Commands
                && let Some((_, line)) = shell_line(&query)
            {
                if !line.trim().is_empty() {
                    close_picker(ed);
                    run_palette_text(ed, &query);
                }
                return;
            }
            if kind == PickerKind::Commands && query.trim().parse::<usize>().is_ok() {
                close_picker(ed);
                run_palette_text(ed, &query);
                return;
            }
            let item = picker.items[item].clone();
            if let ItemData::Dir = item.data {
                // Descend: the directory becomes the query's prefix.
                let prefix = picker.query.get(..picker.needle_start).unwrap_or("").to_string();
                picker.query = format!("{prefix}{}", item.label);
                sync_file_picker(ed);
                return;
            }
            if let ItemData::Argument(path) = &item.data {
                // `cd` goes there; `open` descends into directories and opens files.
                let command = picker.query.split_whitespace().next().unwrap_or("").to_string();
                let descend = path.ends_with('/') && typed::find(&command).is_some_and(|c| c.name != "cd");
                if descend {
                    picker.query = format!("{command} {path}");
                    sync_command_arguments(ed);
                } else {
                    let line = format!("{command} {path}");
                    close_picker(ed);
                    run_palette_text(ed, &line);
                }
                return;
            }
            let Overlay::Picker(picker) = std::mem::replace(&mut ed.overlay, Overlay::None) else { return };
            ed.live_search = None;
            match item.data {
                ItemData::Path(path) => open_path(ed, &path, None, open),
                ItemData::Dir | ItemData::Argument(_) => {}
                ItemData::Location { path, line } => open_path(ed, &path, Some(line), open),
                ItemData::Buffer(id) => show_in(ed, id, open),
                ItemData::Theme(name) => {
                    ed.theme_before_picker = None;
                    typed::set_theme(ed, &name);
                }
                ItemData::Language(name) => typed::set_language(ed, &name),
                ItemData::Command(cmd) => ed.run_command(cmd, commands::Ctx::default()),
                ItemData::Typed(cmd) => {
                    let words: Vec<&str> = query.split_whitespace().collect();
                    // `:open foo.rs` typed in full runs with its arguments.
                    if words.len() > 1 && typed::find(words[0]).is_some_and(|c| c.name == cmd.name) {
                        run_palette_text(ed, &query);
                    } else if cmd.needs_arg {
                        ed.overlay = Overlay::Picker(picker.with_query(&format!("{} ", cmd.name)));
                    } else {
                        typed::run(ed, cmd, &[]);
                    }
                }
            }
        }
    }
}

fn close_picker(ed: &mut Editor) {
    ed.overlay = Overlay::None;
    ed.live_search = None;
}

/// Run a command line typed in the palette (`write foo.txt`, `42`...).
fn run_palette_text(ed: &mut Editor, text: &str) {
    let text = text.trim().trim_start_matches(':');
    if text.is_empty() {
        return;
    }
    // The shell gets the command line untouched (quotes included).
    if let Some((_, line)) = shell_line(text) {
        typed::run_shell(ed, line.trim());
        return;
    }
    if let Ok(line) = text.parse::<usize>() {
        ed.view_mut().save_jump();
        commands::goto_line_number(ed, line.saturating_sub(1), false);
        return;
    }
    let mut words = text.splitn(2, char::is_whitespace);
    let name = words.next().unwrap_or("");
    let args: Vec<String> = words.next().map(typed::split_args).unwrap_or_default();
    match typed::find(name) {
        Some(cmd) => typed::run(ed, cmd, &args),
        None => match commands::find(name) {
            Some(cmd) => ed.run_command(cmd, commands::Ctx::default()),
            None => ed.set_error(format!("unknown command: {name}")),
        },
    }
}

fn show_in(ed: &mut Editor, buffer: BufferId, open: OpenMode) {
    match open {
        OpenMode::Here => ed.show_buffer(buffer),
        OpenMode::VSplit => {
            ed.split(Dir::Horizontal, Some(buffer));
        }
        OpenMode::HSplit => {
            ed.split(Dir::Vertical, Some(buffer));
        }
    }
    if ed.mode.is_editing() {
        ed.set_mode(Mode::Normal);
    }
}

pub fn open_path(ed: &mut Editor, path: &Path, line: Option<usize>, open: OpenMode) {
    match ed.open_file(path) {
        Ok(id) => {
            ed.view_mut().save_jump();
            show_in(ed, id, open);
            if let Some(line) = line {
                commands::goto_line_number(ed, line, false);
            }
        }
        Err(err) => ed.set_error(err),
    }
}
