//! End-to-end tests: feed key sequences to a headless editor and check the
//! resulting text and selection. The key syntax is the one used by the keymap
//! tables (`space w v`, `C-c`...); `text:abc` types characters.

use crate::config::Config;
use crate::editor::{Editor, Mode, Overlay};
use crate::keymap::Key;
use crate::syntax::Registry;
use crate::theme::Theme;

fn editor_with(text: &str) -> Editor {
    let config = Config { auto_pairs: true, ..Config::default() };
    let mut ed = Editor::new(config, Registry::load(None), Theme::builtin("dark").unwrap());
    ed.clipboard = crate::clipboard::Clipboard::in_memory();
    ed.buffer_mut().set_text(text);
    ed.buffer_mut().history = crate::history::History::new();
    let view = ed.view_mut();
    view.rows = 20;
    view.cols = 80;
    ed
}

fn keys(ed: &mut Editor, sequence: &str) {
    for token in sequence.split_whitespace() {
        if let Some(text) = token.strip_prefix("text:") {
            for c in text.chars() {
                ed.handle_key(Key::char(c));
            }
        } else {
            ed.handle_key(Key::parse(token).unwrap_or_else(|| panic!("bad key {token}")));
        }
    }
}

fn text(ed: &Editor) -> String {
    ed.buffer().text.to_string()
}

fn cursor(ed: &Editor) -> usize {
    ed.view().cursor()
}

fn selected(ed: &Editor) -> String {
    ed.view().selection.primary().slice(&ed.buffer().text)
}

#[test]
fn insert_and_escape() {
    let mut ed = editor_with("");
    keys(&mut ed, "i text:hello esc");
    assert_eq!(text(&ed), "hello");
    assert_eq!(ed.mode, Mode::Normal);
    assert_eq!(cursor(&ed), 5);
    keys(&mut ed, "u");
    assert_eq!(text(&ed), "");
    keys(&mut ed, "U");
    assert_eq!(text(&ed), "hello");
}

#[test]
fn word_motions_select_like_helix() {
    let mut ed = editor_with("foo bar baz\n");
    keys(&mut ed, "w");
    assert_eq!(selected(&ed), "foo ");
    keys(&mut ed, "w");
    assert_eq!(selected(&ed), "bar ");
    keys(&mut ed, "b");
    assert_eq!(selected(&ed), "bar ", "b selects back to the word start, whitespace included");
    keys(&mut ed, "e");
    assert_eq!(selected(&ed), "bar");
    keys(&mut ed, "3 w");
    assert_eq!(cursor(&ed), 12, "runs off the end of the text");
}

#[test]
fn delete_yank_and_paste_whole_lines() {
    let mut ed = editor_with("one\ntwo\nthree\n");
    keys(&mut ed, "j x");
    assert_eq!(selected(&ed), "two\n");
    keys(&mut ed, "x");
    assert_eq!(selected(&ed), "two\nthree\n");
    keys(&mut ed, "d");
    assert_eq!(text(&ed), "one\n");
    assert_eq!(ed.clipboard.paste().unwrap(), "two\nthree\n", "deleted text goes to the clipboard");
    keys(&mut ed, "g g x y");
    assert_eq!(ed.clipboard.paste().unwrap(), "one\n");
    keys(&mut ed, "i text:zero ret esc g g x p");
    assert_eq!(text(&ed), "zero\none\none\n", "in normal mode a linewise p goes below the line, replacing nothing");
    assert_eq!(selected(&ed), "one\n", "the pasted line is selected");
    assert_eq!(ed.clipboard.paste().unwrap(), "one\n", "normal-mode paste leaves the clipboard alone");
    keys(&mut ed, "g g x P");
    assert_eq!(text(&ed), "one\nzero\none\none\n", "P goes above the line");
    // In select mode the paste replaces the selected lines, like vim's visual mode.
    keys(&mut ed, "j x v p");
    assert_eq!(text(&ed), "one\none\none\none\n", "select-mode p replaces the whole line");
    assert_eq!(ed.clipboard.paste().unwrap(), "zero\n", "p yanks what it replaced");
    assert_eq!(ed.mode, Mode::Normal, "paste leaves select mode");
    keys(&mut ed, "j x v P");
    assert_eq!(text(&ed), "one\none\nzero\none\n");
    assert_eq!(ed.clipboard.paste().unwrap(), "zero\n", "P leaves the clipboard alone");
    // Below the last line, which has no newline, the paste starts one.
    let mut ed = editor_with("a\nb");
    keys(&mut ed, "x y j x p");
    assert_eq!(text(&ed), "a\nb\na");
}

#[test]
fn paste_inserts_in_normal_mode_and_replaces_in_select_mode() {
    let mut ed = editor_with("alpha beta\n");
    keys(&mut ed, "e y / text:beta ret p");
    assert_eq!(text(&ed), "alpha betaalpha\n", "p pastes after the selection");
    assert_eq!(selected(&ed), "alpha", "the pasted text is selected");
    assert_eq!(ed.clipboard.paste().unwrap(), "alpha", "nothing was replaced, so nothing was yanked");
    keys(&mut ed, "u g g / text:beta ret P");
    assert_eq!(text(&ed), "alpha alphabeta\n", "P pastes before the selection");
    keys(&mut ed, "u g g / text:beta ret v p");
    assert_eq!(text(&ed), "alpha alpha\n", "in select mode p replaces the selection");
    assert_eq!(ed.clipboard.paste().unwrap(), "beta", "the replaced word is now on the clipboard");
    assert_eq!(ed.mode, Mode::Normal);
    keys(&mut ed, "g h e v P");
    assert_eq!(text(&ed), "beta alpha\n", "P replaces too, without yanking");
    assert_eq!(ed.clipboard.paste().unwrap(), "beta");
}

#[test]
fn yank_with_several_cursors_pastes_one_piece_per_cursor() {
    let mut ed = editor_with("a1\nb2\nc3\n");
    keys(&mut ed, "C C e y");
    assert_eq!(ed.clipboard.paste().unwrap(), "a1\nb2\nc3");
    keys(&mut ed, ", g g l P");
    assert_eq!(text(&ed), "aa1\nb2\nc31\nb2\nc3\n", "a single cursor pastes the whole clipboard");
    keys(&mut ed, "u g g l C C P");
    assert_eq!(text(&ed), "aa11\nbb22\ncc33\n", "three cursors get their own piece again");
}

#[test]
fn change_and_append() {
    let mut ed = editor_with("hello world\n");
    keys(&mut ed, "w c text:bye- esc");
    assert_eq!(text(&ed), "bye-world\n");
    keys(&mut ed, "A text:! esc");
    assert_eq!(text(&ed), "bye-world!\n");
    keys(&mut ed, "I text:> space esc");
    assert_eq!(text(&ed), "> bye-world!\n");
    keys(&mut ed, "o text:next esc");
    assert_eq!(text(&ed), "> bye-world!\nnext\n");
    keys(&mut ed, "O text:mid esc");
    assert_eq!(text(&ed), "> bye-world!\nmid\nnext\n");
}

#[test]
fn multiple_cursors_with_copy_selection() {
    let mut ed = editor_with("a\nb\nc\n");
    keys(&mut ed, "C C i text:x esc");
    assert_eq!(text(&ed), "xa\nxb\nxc\n");
    assert_eq!(ed.view().selection.len(), 3);
    keys(&mut ed, ", ;");
    assert_eq!(ed.view().selection.len(), 1);
}

#[test]
fn select_regex() {
    let mut ed = editor_with("cat dog cow\n");
    keys(&mut ed, "% s text:c\\w+ ret");
    assert_eq!(ed.view().selection.len(), 2);
    assert!(matches!(ed.overlay, Overlay::None));
    keys(&mut ed, "A-; ,");
    assert_eq!(selected(&ed), "cow");
}

#[test]
fn search_and_next() {
    let mut ed = editor_with("ab x ab y ab\n");
    keys(&mut ed, "/ text:ab ret");
    assert_eq!(cursor(&ed), 6);
    assert_eq!(selected(&ed), "ab");
    keys(&mut ed, "n");
    assert_eq!(cursor(&ed), 11);
    keys(&mut ed, "n");
    assert_eq!(cursor(&ed), 1, "wraps around");
    keys(&mut ed, "N");
    assert_eq!(cursor(&ed), 11);
    keys(&mut ed, "g g w *");
    assert!(ed.search.regex.as_ref().is_some_and(|r| r.as_str().ends_with("ab ")));
}

#[test]
fn text_objects_and_brackets() {
    let mut ed = editor_with("f(a, (b, c)) end\n");
    keys(&mut ed, "7 l m i (");
    assert_eq!(selected(&ed), "b, c");
    keys(&mut ed, "m a (");
    assert_eq!(selected(&ed), "(b, c)");
    keys(&mut ed, "m i m");
    assert_eq!(selected(&ed), "b, c");
    keys(&mut ed, "; m m");
    assert_eq!(cursor(&ed), 10, "mm from inside a pair goes to its closing bracket");
    keys(&mut ed, "m m");
    assert_eq!(cursor(&ed), 5, "and back to the opening one");
    let mut ed = editor_with("x = [1, {2, 3}]\n");
    keys(&mut ed, "9 l m m");
    assert_eq!(cursor(&ed), 13, "brace pairs match too");
    keys(&mut ed, "g h m m");
    assert_eq!(cursor(&ed), 0, "nothing to match: stay");
    keys(&mut ed, "4 l m m");
    assert_eq!(cursor(&ed), 14, "[] pairs match");
}

#[test]
fn indent_and_comment() {
    let mut ed = editor_with("fn main() {\nlet a = 1;\nlet b = 2;\n}\n");
    let rust = ed.registry.find("rust");
    ed.buffer_mut().set_language(rust);
    keys(&mut ed, "j x x >");
    assert_eq!(text(&ed), "fn main() {\n    let a = 1;\n    let b = 2;\n}\n");
    keys(&mut ed, "C-c");
    assert_eq!(text(&ed), "fn main() {\n    // let a = 1;\n    // let b = 2;\n}\n");
    keys(&mut ed, "C-c <");
    assert_eq!(text(&ed), "fn main() {\nlet a = 1;\nlet b = 2;\n}\n");
}

#[test]
fn insert_mode_helpers() {
    let mut ed = editor_with("");
    let rust = ed.registry.find("rust");
    ed.buffer_mut().set_language(rust);
    keys(&mut ed, "i text:fn text:( text:) space text:{ ret text:x esc");
    assert_eq!(text(&ed), "fn() {\n    x\n}");
    keys(&mut ed, "A backspace backspace text:y esc");
    assert_eq!(text(&ed), "fn() {\ny\n}", "backspace in leading spaces removes one indent level");
    keys(&mut ed, "g g i del del esc");
    assert_eq!(text(&ed), "() {\ny\n}");
}

#[test]
fn replace_char_and_replace_mode() {
    let mut ed = editor_with("Hello\n");
    keys(&mut ed, "w g h r z");
    assert_eq!(text(&ed), "zello\n", "r only changes the character under the cursor");
    assert_eq!(cursor(&ed), 0);
    keys(&mut ed, "l R text:ipp esc");
    assert_eq!(text(&ed), "zippo\n");
    assert_eq!(ed.mode, Mode::Normal);
    keys(&mut ed, "g l R text:y!! esc");
    assert_eq!(text(&ed), "zippy!!\n", "replace mode inserts once the line runs out");
    keys(&mut ed, "u");
    assert_eq!(text(&ed), "zippo\n", "one undo step per replace session");
    keys(&mut ed, "j R text:x esc");
    assert_eq!(text(&ed), "zippo\nx", "the empty last line simply gets the text");
}

#[test]
fn repeat_last_insert_with_dot() {
    let mut ed = editor_with("a\nb\n");
    keys(&mut ed, "A text:! esc j .");
    assert_eq!(text(&ed), "a!\nb!\n");
}

#[test]
fn frames_tabs_and_typed_commands() {
    let mut ed = editor_with("l1\nl2\nl3\n");
    keys(&mut ed, ": text:3 ret");
    assert_eq!(ed.buffer().line_of(cursor(&ed)), 2);
    keys(&mut ed, "space w v");
    assert_eq!(ed.root().leaves().len(), 2);
    keys(&mut ed, ": text:quit ret");
    assert_eq!(ed.root().leaves().len(), 1);
    keys(&mut ed, "space w s space w k space w j space w q");
    assert_eq!(ed.root().leaves().len(), 1);
    keys(&mut ed, "space w n space w N");
    assert_eq!(ed.root().leaves().len(), 3);
    let first = ed.focused_view();
    keys(&mut ed, "space w w");
    assert_ne!(ed.focused_view(), first);
    keys(&mut ed, "space w W");
    assert_eq!(ed.focused_view(), first);
    keys(&mut ed, "space w o");
    assert_eq!(ed.root().leaves().len(), 1);
    keys(&mut ed, "space w t");
    assert_eq!(ed.tabs.len(), 2);
    assert_eq!(ed.tab_index, 1);
    keys(&mut ed, "g t");
    assert_eq!(ed.tab_index, 0, "gt wraps to the first tab");
    keys(&mut ed, "g T");
    assert_eq!(ed.tab_index, 1);
    keys(&mut ed, "space w x");
    assert_eq!(ed.tabs.len(), 1);
    keys(&mut ed, ": text:pwd ret");
    assert_eq!(ed.message.as_ref().map(|m| m.text.clone()), Some(ed.cwd.display().to_string()));
    keys(&mut ed, ": text:new ret");
    assert_eq!(text(&ed), "");
    keys(&mut ed, "g p i text:dirty esc : text:quit ret");
    assert!(!ed.quit, "quit refused with unsaved changes");
    keys(&mut ed, ": text:quit! ret");
    assert!(ed.quit);
}

#[test]
fn write_and_reload_file() {
    let dir = std::env::temp_dir().join(format!("quin-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("note.md");
    let mut ed = editor_with("");
    keys(&mut ed, "i text:# space text:hi esc");
    keys(&mut ed, &format!(": text:write space text:{} ret", path.display()));
    assert!(ed.message.as_ref().map(|m| m.text.clone()).unwrap_or_default().contains("wrote"), "{:?}", ed.message);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "# hi");
    assert!(!ed.buffer().is_modified());
    assert_eq!(ed.buffer().language, ed.registry.find("markdown"));
    let id = ed.open_file(&path).unwrap();
    assert_eq!(id, ed.view().buffer, "opening the same file reuses the buffer");
    std::fs::write(&path, "# changed").unwrap();
    keys(&mut ed, ": text:reload ret");
    assert_eq!(text(&ed), "# changed");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn file_pickers_respect_gitignore_unless_asked() {
    let dir = std::env::temp_dir().join(format!("quin-pick-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("build")).unwrap();
    std::fs::write(dir.join(".gitignore"), "build/\n").unwrap();
    std::fs::write(dir.join("keep.rs"), "").unwrap();
    std::fs::write(dir.join("build/out.o"), "").unwrap();
    let mut ed = editor_with("");
    keys(&mut ed, &format!(": text:cd space text:{} ret", dir.display()));
    keys(&mut ed, "space e");
    let Overlay::Picker(picker) = &ed.overlay else { panic!("no picker") };
    let labels: Vec<&str> = picker.items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, vec!["keep.rs"]);
    keys(&mut ed, "esc space E");
    let Overlay::Picker(picker) = &ed.overlay else { panic!("no picker") };
    let mut labels: Vec<&str> = picker.items.iter().map(|i| i.label.as_str()).collect();
    labels.sort();
    assert_eq!(labels, vec![".gitignore", "build/out.o", "keep.rs"]);
    keys(&mut ed, "esc");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn select_mode_extends_and_undo_restores_selection() {
    let mut ed = editor_with("abc def\n");
    keys(&mut ed, "v w");
    assert_eq!(ed.mode, Mode::Select);
    assert_eq!(selected(&ed), "abc ");
    keys(&mut ed, "esc l l v l d");
    assert_eq!(text(&ed), "abc d\n");
    keys(&mut ed, "u");
    assert_eq!(text(&ed), "abc def\n");
    assert_eq!(selected(&ed), "ef");
}

#[test]
fn jumps_and_goto() {
    let mut ed = editor_with("1\n2\n3\n4\n5\n");
    keys(&mut ed, "g e");
    assert_eq!(cursor(&ed), ed.buffer().len_chars());
    keys(&mut ed, "C-o");
    assert_eq!(ed.buffer().line_of(cursor(&ed)), 0);
    keys(&mut ed, "C-i");
    assert_eq!(cursor(&ed), ed.buffer().len_chars());
    keys(&mut ed, "3 g g");
    assert_eq!(ed.buffer().line_of(cursor(&ed)), 2);
    keys(&mut ed, "g g C-d");
    assert_eq!(ed.buffer().line_of(cursor(&ed)), 5, "half a page of 20 rows is 10 lines, clamped to the end");
    let mut ed = editor_with(&"line\n".repeat(200));
    keys(&mut ed, "C-d");
    assert_eq!(ed.view().scroll_line, 10);
    assert_eq!(ed.buffer().line_of(cursor(&ed)), 13, "the cursor lands inside the scroll-off band");
    keys(&mut ed, "C-u");
    assert_eq!(ed.view().scroll_line, 0);
    assert_eq!(ed.buffer().line_of(cursor(&ed)), 3, "the cursor keeps its row in the view");
}

#[test]
fn terminal_is_a_plain_buffer_with_a_mode() {
    let mut ed = editor_with("code\n");
    keys(&mut ed, ": text:terminal ret");
    if ed.message.as_ref().is_some_and(|m| m.text.contains("cannot start terminal")) {
        return; // no pty available (CI sandbox)
    }
    assert_eq!(ed.mode, Mode::Terminal);
    assert_eq!(ed.root().leaves().len(), 1, ":terminal opens in the current frame");
    keys(&mut ed, "C-esc g p");
    assert_eq!(text(&ed), "code\n", "the file is still a buffer, like any other");
    keys(&mut ed, "space w v g n");
    assert!(ed.buffer().is_terminal());
    assert_eq!(ed.mode, Mode::Normal);
    keys(&mut ed, "i");
    assert_eq!(ed.mode, Mode::Terminal);
    assert_eq!(ed.root().leaves().len(), 2);
    keys(&mut ed, "C-esc");
    assert_eq!(ed.mode, Mode::Normal);
    keys(&mut ed, "i");
    assert_eq!(ed.mode, Mode::Terminal, "insert on a terminal goes back to terminal mode");
    keys(&mut ed, "C-esc space w w");
    assert_eq!(ed.mode, Mode::Normal);
    assert_eq!(text(&ed), "code\n", "the other frame still shows the file");
    keys(&mut ed, "space w w");
    assert!(ed.buffer().is_terminal());
    assert_eq!(ed.mode, Mode::Normal, "focusing a terminal frame does not force terminal mode");
}

#[test]
fn file_picker_browses_any_directory() {
    let dir = std::env::temp_dir().join(format!("quin-browse-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("sub/deep.txt"), "deep").unwrap();
    std::fs::write(dir.join("top.txt"), "top").unwrap();
    let mut ed = editor_with("");
    keys(&mut ed, &format!("space e text:{}/", dir.display()));
    let Overlay::Picker(picker) = &ed.overlay else { panic!("no picker") };
    let labels: Vec<&str> = picker.items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, vec!["../", "sub/", "top.txt"], "directories first, then files");
    keys(&mut ed, "text:su ret");
    let Overlay::Picker(picker) = &ed.overlay else { panic!("picker closed") };
    assert_eq!(picker.query, format!("{}/sub/", dir.display()), "choosing a directory descends into it");
    assert_eq!(picker.items.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(), vec!["../", "deep.txt"]);
    keys(&mut ed, "text:de ret");
    assert!(matches!(ed.overlay, Overlay::None));
    assert_eq!(text(&ed), "deep");
    keys(&mut ed, &format!("space e text:{}/new.md ret", dir.display()));
    assert_eq!(ed.buffer().path.as_deref(), Some(dir.join("new.md").as_path()), "a path that does not exist yet opens as a new file");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn global_search_fills_in_the_background() {
    let dir = std::env::temp_dir().join(format!("quin-grep-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.txt"), "nothing\nneedle here\n").unwrap();
    std::fs::write(dir.join("b.txt"), "  another Needle\n").unwrap();
    let mut ed = editor_with("");
    keys(&mut ed, &format!(": text:cd space text:{} ret space f text:needle", dir.display()));
    for _ in 0..200 {
        ed.poll_search();
        if ed.live_search.as_ref().is_some_and(|s| s.done) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let Overlay::Picker(picker) = &ed.overlay else { panic!("no picker") };
    let mut labels: Vec<&str> = picker.items.iter().map(|i| i.label.as_str()).collect();
    labels.sort();
    assert_eq!(labels, vec!["a.txt:2: needle here", "b.txt:1: another Needle"], "smart case, trimmed lines");
    let hit = picker.items.iter().find(|i| i.label.starts_with("b.txt")).unwrap();
    assert_eq!(hit.highlight, (17..23).collect::<Vec<_>>(), "the match is highlighted after the `path:line: ` prefix");
    keys(&mut ed, "ret");
    assert!(ed.live_search.is_none());
    let line = ed.buffer().line_of(cursor(&ed));
    assert!(ed.buffer().line_text(line).to_lowercase().contains("needle"), "ret jumps to the matching line");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn space_c_closes_the_frame() {
    let mut ed = editor_with("");
    keys(&mut ed, "space w v space c");
    assert_eq!(ed.root().leaves().len(), 1);
}

#[test]
fn zoom_changes_the_font_size() {
    let mut ed = editor_with("");
    let base = Config::default().font_size;
    keys(&mut ed, "C-= C-+ C--");
    assert_eq!(ed.config.font_size, base + 1.0);
    assert!(ed.font_dirty);
    keys(&mut ed, "i C--");
    assert_eq!(ed.config.font_size, base, "zoom works in insert mode too");
}

#[test]
fn tree_sidebar_opens_folds_and_edits_files() {
    let dir = std::env::temp_dir().join(format!("quin-tree-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/lib.rs"), "lib").unwrap();
    std::fs::write(dir.join("notes.md"), "notes").unwrap();
    std::fs::write(dir.join(".env"), "secret").unwrap();
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    let mut ed = editor_with("");
    keys(&mut ed, &format!(": text:cd space text:{} ret A-e", dir.display()));
    assert_eq!(ed.root().leaves().len(), 2);
    assert!(ed.buffer().tree);
    assert_eq!(text(&ed), "src/\nnotes.md\n", "dot-files start hidden");
    keys(&mut ed, ".");
    assert_eq!(text(&ed), ".git/\nsrc/\n.env\nnotes.md\n", ". shows every dot entry, .git included");
    keys(&mut ed, ".");
    assert_eq!(text(&ed), "src/\nnotes.md\n", ". hides them again");
    keys(&mut ed, "ret");
    assert_eq!(text(&ed), "src/\n  lib.rs\nnotes.md\n", "ret on a directory expands it");
    keys(&mut ed, "tab");
    assert_eq!(text(&ed), "src/\nnotes.md\n", "tab folds it again");
    keys(&mut ed, "tab");
    assert_eq!(text(&ed), "src/\n  lib.rs\nnotes.md\n");
    keys(&mut ed, "j tab k");
    assert_eq!(text(&ed), "src/\n  lib.rs\nnotes.md\n", "tab on a file does nothing");
    assert!(ed.buffer().tree);
    // S-ret makes the entry's directory the working directory; the tree follows.
    keys(&mut ed, "S-ret");
    assert_eq!(ed.cwd, dir.join("src"));
    assert_eq!(text(&ed), "lib.rs\n", "the tree now shows the new working directory");
    assert_eq!(ed.root().leaves().len(), 2, "nothing closed");
    keys(&mut ed, "S-ret");
    assert_eq!(ed.cwd, dir.join("src"), "on a file: its directory, which is the same here");
    keys(&mut ed, &format!(": text:cd space text:{} ret", dir.display()));
    assert_eq!(text(&ed), "src/\nnotes.md\n", ":cd re-renders the tree too");
    keys(&mut ed, "g g ret");
    assert_eq!(text(&ed), "src/\n  lib.rs\nnotes.md\n");
    // `-` shows the parent without changing the working directory; reopening resets.
    let name = dir.file_name().unwrap().to_string_lossy().into_owned();
    keys(&mut ed, "-");
    assert_eq!(ed.cwd, dir, "cwd unchanged");
    let listing = text(&ed);
    assert!(
        listing.contains(&format!("{name}/\n  src/\n    lib.rs\n  notes.md\n")),
        "the directory we came from stays unfolded: {listing}"
    );
    keys(&mut ed, "A-e A-e");
    assert_eq!(text(&ed), "src/\nnotes.md\n", "reopened: back at the working directory");
    keys(&mut ed, "g g ret");
    assert_eq!(text(&ed), "src/\n  lib.rs\nnotes.md\n");
    keys(&mut ed, "j ret");
    assert!(!ed.buffer().tree, "a file opens in the other frame");
    assert_eq!(text(&ed), "lib");
    assert_eq!(ed.root().leaves().len(), 2, "the tree stays open");
    keys(&mut ed, "A-e");
    assert_eq!(ed.root().leaves().len(), 1, "A-e hides the sidebar");
    keys(&mut ed, "A-e");
    assert!(ed.buffer().tree);
    // Create a file and a directory by editing, then rename and delete.
    keys(&mut ed, "g e O text:new.txt esc o text:docs/ esc : text:write ret");
    assert!(dir.join("new.txt").is_file(), "{:?}", ed.message);
    assert!(dir.join("docs").is_dir());
    assert!(!ed.buffer().is_modified(), "the tree is re-rendered after writing");
    keys(&mut ed, "/ text:notes ret c text:notes2 esc : text:write ret");
    assert!(dir.join("notes2.md").is_file() && !dir.join("notes.md").exists(), "{:?}", ed.message);
    keys(&mut ed, "/ text:new.txt ret x d : text:write ret");
    assert!(dir.join("new.txt").exists(), "the first write only announces the deletion");
    keys(&mut ed, ": text:write ret");
    assert!(!dir.join("new.txt").exists(), "the second write deletes");
    // `o` on an unfolded directory starts the new line inside it, so a new
    // directory lands in src/ and src/'s files stay where they are.
    keys(&mut ed, "g g / text:src/ ret");
    assert!(text(&ed).contains("src/\n  lib.rs\n"), "{}", text(&ed));
    keys(&mut ed, "o text:test/ esc : text:write ret");
    assert!(dir.join("src/test").is_dir(), "{:?}", ed.message);
    assert!(dir.join("src/lib.rs").is_file() && !dir.join("test").exists());
    assert!(text(&ed).contains("src/\n  test/\n  lib.rs\n"), "{}", text(&ed));
    // Typed unindented between src/ and its children, the write is refused.
    keys(&mut ed, "g g / text:src/ ret A ret text:other/ esc : text:write ret");
    assert!(!dir.join("other").exists() && dir.join("src/lib.rs").is_file(), "{:?}", ed.message);
    assert!(format!("{:?}", ed.message).contains("would take the entries of `src/`"), "{:?}", ed.message);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn capital_x_selects_lines_upwards() {
    let mut ed = editor_with("one\ntwo\nthree\n");
    keys(&mut ed, "j j X");
    assert_eq!(selected(&ed), "three\n");
    keys(&mut ed, "X");
    assert_eq!(selected(&ed), "two\nthree\n");
    assert_eq!(ed.buffer().line_of(cursor(&ed)), 1, "the cursor sits on the top line");
    keys(&mut ed, "X X");
    assert_eq!(selected(&ed), "one\ntwo\nthree\n", "stops at the first line");
    keys(&mut ed, "d");
    assert_eq!(text(&ed), "");
}

#[test]
fn frames_resize_and_the_menu_stays_open() {
    let mut ed = editor_with("");
    keys(&mut ed, "space w v");
    let widths = |ed: &Editor| {
        let mut rects = Vec::new();
        ed.root().layout(crate::render::Rect::new(0, 0, 1002, 502), &mut rects);
        rects.iter().map(|(_, r)| r.w).collect::<Vec<_>>()
    };
    assert_eq!(widths(&ed), vec![500, 500]);
    keys(&mut ed, "space w > >");
    assert_eq!(widths(&ed), vec![400, 600], "two presses without retyping the menu");
    assert_eq!(ed.pending_keys.len(), 2, "the window menu is still open");
    keys(&mut ed, "< esc");
    assert_eq!(widths(&ed), vec![450, 550]);
    assert!(ed.pending_keys.is_empty());
    keys(&mut ed, "space w v space w q");
    assert_eq!(widths(&ed), vec![500, 500], "a split (and its close) equalises the frames");
    keys(&mut ed, "space w +");
    assert!(ed.message.as_ref().is_some_and(|m| m.text.contains("no frame")), "nothing stacked to resize against");
    keys(&mut ed, "esc space w s space w - esc");
    let mut rects = Vec::new();
    ed.root().layout(crate::render::Rect::new(0, 0, 1002, 502), &mut rects);
    let focused = rects.iter().find(|(v, _)| *v == ed.focused_view()).unwrap().1;
    assert_eq!(focused.h, 225, "shorter by a twentieth of the split");
}

#[test]
fn bundled_themes_switch_at_runtime() {
    let mut ed = editor_with("");
    keys(&mut ed, ": text:theme space text:catppuccin-mocha ret");
    assert_eq!(ed.config.theme, "catppuccin-mocha");
    assert_eq!(ed.theme.color("ui.background"), crate::render::Color::from_hex("1e1e2e").unwrap());
    keys(&mut ed, ": text:theme space text:no-such-theme ret");
    assert!(ed.message.as_ref().is_some_and(|m| m.text.contains("unknown theme")));
}

#[test]
fn resizing_only_moves_the_shared_edge() {
    let mut ed = editor_with("");
    keys(&mut ed, "space w v space w v");
    // Widths in a 1000-pixel row, ignoring the one-pixel separators.
    let widths = |ed: &Editor, expected: &[i32], why: &str| {
        let mut rects = Vec::new();
        let separators = crate::frame::SEPARATOR * (expected.len() as i32 - 1);
        ed.root().layout(crate::render::Rect::new(0, 0, 1000 + separators, 500), &mut rects);
        let got: Vec<i32> = rects.iter().map(|(_, r)| r.w).collect();
        assert!(got.len() == expected.len() && got.iter().zip(expected).all(|(g, e)| (g - e).abs() <= 1), "{why}: {got:?} vs {expected:?}");
    };
    widths(&ed, &[333, 333, 333], "frames share the space equally after a split");
    keys(&mut ed, "space w > esc");
    widths(&ed, &[333, 283, 383], "C grows into B; A keeps its size");
    keys(&mut ed, "space w W space w < esc");
    widths(&ed, &[333, 233, 433], "B shrinks, C takes the space");
    keys(&mut ed, "space w 0");
    widths(&ed, &[333, 333, 333], "space w 0 makes the frames equal");
    keys(&mut ed, "space w > esc space w w space c");
    widths(&ed, &[500, 500], "closing a frame equalises the rest");
}

#[test]
fn theme_picker_previews_and_rolls_back() {
    let mut ed = editor_with("");
    let before = ed.theme.color("ui.background");
    keys(&mut ed, ": text:theme ret text:catppuccin-mocha");
    assert!(matches!(ed.overlay, Overlay::Picker(_)));
    assert_eq!(ed.theme.color("ui.background"), crate::render::Color::from_hex("1e1e2e").unwrap(), "typing previews the match");
    keys(&mut ed, "esc");
    assert_eq!(ed.theme.color("ui.background"), before, "escape rolls back");
    assert_eq!(ed.config.theme, Config::default().theme);
    keys(&mut ed, ": text:theme ret text:gruvbox-dark ret");
    assert_eq!(ed.config.theme, "gruvbox-dark");
    assert_ne!(ed.theme.color("ui.background"), before);
}

#[test]
fn scroll_jumps_slide_when_animation_is_on() {
    let mut ed = editor_with(&"line\n".repeat(200));
    keys(&mut ed, "C-d");
    let view = ed.view_mut();
    assert_eq!(view.scroll_line, 10);
    assert!(view.anim.is_some(), "a half-page jump animates");
    let shown = view.displayed_scroll(120);
    assert!(shown < 10.0, "right after the jump the view is still near the top: {shown}");
    std::thread::sleep(std::time::Duration::from_millis(150));
    assert_eq!(ed.view_mut().displayed_scroll(120), 10.0);
    assert!(ed.view().anim.is_none(), "finished animations are dropped");
    ed.config.scroll_animation = 0;
    keys(&mut ed, "C-d");
    assert!(ed.view().anim.is_none(), "scroll-animation = 0 jumps at once");
}

#[test]
fn cursor_slides_to_its_new_place() {
    let mut view = crate::frame::View::new(crate::frame::ViewId(1), crate::buffer::BufferId(1));
    assert_eq!(view.cursor_position((0.0, 0.0), 80), (0.0, 0.0), "the first draw places it directly");
    let shown = view.cursor_position((100.0, 40.0), 80);
    assert!(shown.0 < 100.0 && view.cursor_anim.is_some(), "a move starts a slide: {shown:?}");
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert_eq!(view.cursor_position((100.0, 40.0), 80), (100.0, 40.0));
    assert!(view.cursor_anim.is_none());
    assert_eq!(view.cursor_position((0.0, 0.0), 0), (0.0, 0.0), "cursor-animation = 0 jumps");
    assert!(view.cursor_anim.is_none());
}

#[test]
fn palette_completes_paths_for_cd_and_open() {
    let dir = std::env::temp_dir().join(format!("quin-cd-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("alpha/inner")).unwrap();
    std::fs::create_dir_all(dir.join("beta")).unwrap();
    std::fs::write(dir.join("notes.txt"), "n").unwrap();
    let mut ed = editor_with("");
    keys(&mut ed, &format!(": text:cd space text:{} ret", dir.display()));
    keys(&mut ed, ": text:cd space");
    let Overlay::Picker(picker) = &ed.overlay else { panic!("no picker") };
    let labels: Vec<&str> = picker.items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, vec!["alpha/", "beta/"], "cd lists only directories");
    keys(&mut ed, "text:al tab");
    let Overlay::Picker(picker) = &ed.overlay else { panic!("no picker") };
    assert_eq!(picker.query, "cd alpha/", "tab completes and goes on listing inside");
    assert_eq!(picker.items.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(), vec!["inner/"]);
    keys(&mut ed, "ret");
    assert!(matches!(ed.overlay, Overlay::None));
    assert_eq!(ed.cwd, dir.join("alpha").join("inner"), "ret runs cd with the chosen directory");
    keys(&mut ed, &format!(": text:cd space text:{} ret", dir.display()));
    keys(&mut ed, ": text:open space");
    let Overlay::Picker(picker) = &ed.overlay else { panic!("no picker") };
    let labels: Vec<&str> = picker.items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, vec!["alpha/", "beta/", "notes.txt"], "open lists files too");
    keys(&mut ed, "text:not ret");
    assert_eq!(text(&ed), "n");
    keys(&mut ed, ": text:th");
    let Overlay::Picker(picker) = &ed.overlay else { panic!("no picker") };
    assert!(picker.items.iter().any(|i| i.label == "theme"), "back to the command list without an argument");
    keys(&mut ed, "esc");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn capital_j_joins_lines_like_vim() {
    let mut ed = editor_with("fn x() {\n    a();\n    b();\n}\n");
    keys(&mut ed, "J");
    assert_eq!(text(&ed), "fn x() { a();\n    b();\n}\n");
    assert_eq!(cursor(&ed), 8, "the cursor sits on the joining space");
    keys(&mut ed, "2 J");
    assert_eq!(text(&ed), "fn x() { a(); b(); }\n");
    let mut ed = editor_with("a\n\nb\nc\n");
    keys(&mut ed, "J");
    assert_eq!(text(&ed), "a\nb\nc\n", "an empty line joins without a space");
    keys(&mut ed, "x x J");
    assert_eq!(text(&ed), "a b\nc\n", "a two-line selection joins those two lines");
    keys(&mut ed, "g g x x x J");
    assert_eq!(text(&ed), "a b c\n", "a selection over all lines joins them all");
    keys(&mut ed, "g e J");
    assert_eq!(text(&ed), "a b c\n", "nothing below: nothing happens");
}

#[test]
fn cd_and_open_expand_tilde() {
    let home = std::env::var("HOME").unwrap();
    let mut ed = editor_with("");
    keys(&mut ed, ": text:cd space text:~ ret");
    assert_eq!(ed.cwd, std::path::PathBuf::from(&home));
    assert_eq!(crate::files::expand_home("~/x"), std::path::PathBuf::from(&home).join("x"));
    assert_eq!(crate::files::expand_home("~x"), std::path::PathBuf::from("~x"));
}

#[test]
fn an_exited_shell_closes_its_frame() {
    let mut ed = editor_with("code\n");
    keys(&mut ed, "space w s : text:terminal ret");
    if ed.message.as_ref().is_some_and(|m| m.text.contains("cannot start terminal")) {
        return; // no pty available
    }
    assert_eq!(ed.root().leaves().len(), 2);
    keys(&mut ed, "text:exit ret");
    for _ in 0..100 {
        std::thread::sleep(std::time::Duration::from_millis(30));
        if ed.process_terminals() && ed.root().leaves().len() == 1 {
            break;
        }
    }
    assert_eq!(ed.root().leaves().len(), 1, "the terminal frame is gone");
    assert!(!ed.buffer().is_terminal());
    assert_eq!(text(&ed), "code\n", "back to the file, in normal mode");
    assert_eq!(ed.mode, Mode::Normal);
    assert!(ed.buffers.values().all(|b| !b.is_terminal()), "the buffer is closed too");
}

#[test]
fn git_status_marks_the_tree_and_the_status_line() {
    let git = |dir: &std::path::Path, args: &[&str]| {
        std::process::Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).output()
    };
    let dir = std::env::temp_dir().join(format!("quin-git-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let dir = dir.canonicalize().unwrap();
    if git(&dir, &["init", "-q"]).is_err() {
        return; // no git on this machine
    }
    std::fs::write(dir.join("sub/keep.rs"), "a").unwrap();
    std::fs::write(dir.join("top.txt"), "b").unwrap();
    git(&dir, &["add", "."]).unwrap();
    git(&dir, &["commit", "-q", "-m", "init"]).unwrap();
    let clean = crate::git::query(&dir).unwrap();
    assert!(!clean.dirty() && !clean.branch.is_empty());
    std::fs::write(dir.join("sub/keep.rs"), "changed").unwrap();
    std::fs::write(dir.join("new.txt"), "n").unwrap();
    let state = crate::git::query(&dir).unwrap();
    assert_eq!(state.label(), format!("{}*", clean.branch));
    assert_eq!(state.status(&dir.join("sub/keep.rs")), Some(crate::git::FileStatus::Modified));
    assert_eq!(state.status(&dir.join("new.txt")), Some(crate::git::FileStatus::New));
    assert_eq!(state.status(&dir.join("top.txt")), None);
    assert!(state.dir_changed(&dir.join("sub")) && state.dir_changed(&dir));
    let mut ed = editor_with("");
    keys(&mut ed, &format!(": text:cd space text:{} ret", dir.display()));
    ed.git.state = Some(state);
    keys(&mut ed, "A-e");
    assert_eq!(text(&ed), "* sub/\nnew.txt\ntop.txt\n", "changed directories carry a marker");
    keys(&mut ed, "ret");
    assert_eq!(text(&ed), "* sub/\n  keep.rs\nnew.txt\ntop.txt\n");
    keys(&mut ed, "G e O text:more.txt esc : text:write ret");
    assert!(dir.join("more.txt").is_file(), "editing a line with a marker still works: {:?}", ed.message);
    // The status buffer: its own tab, sections, ret opens the file.
    keys(&mut ed, "space l");
    for _ in 0..200 {
        ed.poll_git(false);
        if !ed.git.in_flight() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(ed.tabs.len(), 2);
    assert!(ed.buffer().git && ed.buffer().readonly);
    let status = text(&ed);
    assert!(status.starts_with(&format!("Head:  {}\n", clean.branch)), "{status}");
    assert!(status.contains("Untracked files (2)\n> untracked    more.txt\n> untracked    new.txt\n"), "{status}");
    assert!(status.contains("Unstaged changes (1)\n> modified     sub/keep.rs\n"), "{status}");
    // ret opens the file in a split on the right; the status buffer keeps the focus.
    keys(&mut ed, "/ text:keep ret ret");
    assert_eq!(ed.root().leaves().len(), 2);
    let file_view = ed.git_buffer.as_ref().unwrap().file_view.unwrap();
    assert_eq!(ed.focused_view(), file_view, "ret moves into the preview");
    assert_eq!(text(&ed), "changed");
    assert!(ed.view().readonly, "the preview split is read-only");
    keys(&mut ed, "i text:x esc");
    assert_eq!(text(&ed), "changed", "typing in the preview is refused");
    keys(&mut ed, "space w w / text:new.txt ret ret");
    assert_eq!(ed.root().leaves().len(), 2, "the same split is reused");
    assert_eq!((ed.focused_view(), text(&ed)), (file_view, "n".to_string()));
    let marks = crate::git::marks_for(&ed, file_view);
    assert!(marks.is_some_and(|m| m.lines.get(&0).is_some_and(|c| c.added)), "an untracked file is all new lines");
    keys(&mut ed, "space w w esc");
    assert_eq!(ed.root().leaves().len(), 1, "esc in the status buffer closes the preview");
    keys(&mut ed, "esc");
    assert_eq!(ed.root().leaves().len(), 1);
    // tab unfolds the diff, space stages the hunk.
    keys(&mut ed, "/ text:keep ret tab");
    let status = text(&ed);
    assert!(
        status.contains("v modified     sub/keep.rs\n    @@") && status.contains("    -a") && status.contains("    +changed"),
        "{status}"
    );
    keys(&mut ed, "j j s");
    for _ in 0..300 {
        ed.poll_git(false);
        if text(&ed).contains("Staged changes (1)") && !ed.git.in_flight() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let status = text(&ed);
    assert!(status.contains("Staged changes (1)\n> modified     sub/keep.rs\n"), "the hunk is staged: {status}");
    assert!(!status.contains("Unstaged changes"), "{status}");
    // s on a section heading stages (or unstages) everything under it.
    keys(&mut ed, "/ text:Untracked ret s");
    for _ in 0..300 {
        ed.poll_git(false);
        if text(&ed).contains("Staged changes (3)") && !ed.git.in_flight() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(text(&ed).contains("> new file     more.txt") && text(&ed).contains("> new file     new.txt"), "{}", text(&ed));
    assert!(!text(&ed).contains("Untracked files"), "{}", text(&ed));
    keys(&mut ed, "/ text:Staged space text:changes ret s");
    for _ in 0..300 {
        ed.poll_git(false);
        if !text(&ed).contains("Staged changes") && !ed.git.in_flight() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(text(&ed).contains("Untracked files (2)") && text(&ed).contains("Unstaged changes (1)"), "everything unstaged: {}", text(&ed));
    keys(&mut ed, "/ text:Unstaged ret s / text:more.txt ret s");
    for _ in 0..300 {
        ed.poll_git(false);
        if text(&ed).contains("Staged changes (2)") && !ed.git.in_flight() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(text(&ed).contains("> new file     more.txt"), "{}", text(&ed));
    keys(&mut ed, "space l");
    assert_eq!((ed.tabs.len(), ed.tab_index), (2, 1), "space l goes back to the status tab");
    // c c opens a floating commit message; :write commits it.
    keys(&mut ed, "c c");
    assert!(ed.float.is_some() && ed.mode == Mode::Insert, "{:?}", ed.message);
    assert_eq!(ed.buffer().commit, Some(false));
    assert!(text(&ed).contains("# Changes to be committed:\n") && text(&ed).contains("#\tmodified:   sub/keep.rs\n"), "{}", text(&ed));
    keys(&mut ed, "text:Stage space text:things esc : text:write-quit ret");
    assert!(ed.float.is_none(), "{:?}", ed.message);
    assert!(ed.buffer().git, "back in the status buffer: write-quit only left the commit frame");
    assert_eq!(ed.tabs.len(), 2);
    let subject = git(&dir, &["log", "-1", "--pretty=%s"]).unwrap();
    assert_eq!(String::from_utf8_lossy(&subject.stdout).trim(), "Stage things");
    // P pushes (setting the upstream the first time), p pulls; both in the background.
    let bare = std::env::temp_dir().join(format!("quin-git-{}-remote", std::process::id()));
    std::fs::create_dir_all(&bare).unwrap();
    git(&bare, &["init", "-q", "--bare"]).unwrap();
    git(&dir, &["remote", "add", "origin", &bare.to_string_lossy()]).unwrap();
    let wait_remote = |ed: &mut Editor| {
        for _ in 0..600 {
            ed.poll_git(false);
            if ed.git.remote.is_none() && !ed.git.in_flight() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    };
    keys(&mut ed, "P");
    let op = ed.git.remote.as_ref().expect("push runs in the background");
    assert!(op.indicator().starts_with('↑'), "the status line shows an up arrow and a spinner: {}", op.indicator());
    wait_remote(&mut ed);
    assert!(ed.message.as_ref().is_some_and(|m| m.text.starts_with("git push:")), "{:?}", ed.message);
    let pushed = git(&bare, &["log", "-1", "--pretty=%s", &clean.branch]).unwrap();
    assert_eq!(String::from_utf8_lossy(&pushed.stdout).trim(), "Stage things", "the commit reached the remote");
    keys(&mut ed, "p");
    wait_remote(&mut ed);
    assert!(ed.message.as_ref().is_some_and(|m| m.text.contains("Already up to date")), "{:?}", ed.message);
    // The status query counts commits ahead of and behind the upstream (locally, at once).
    assert_eq!(crate::git::count_sync(&dir), Some(crate::git::Sync { ahead: 0, behind: 0 }));
    std::fs::write(dir.join("top.txt"), "later").unwrap();
    git(&dir, &["commit", "-q", "-am", "local only"]).unwrap();
    assert_eq!(crate::git::count_sync(&dir), Some(crate::git::Sync { ahead: 1, behind: 0 }));
    ed.poll_git(true);
    for _ in 0..600 {
        ed.poll_git(false);
        if ed.git.state.as_ref().and_then(|s| s.sync) == Some(crate::git::Sync { ahead: 1, behind: 0 }) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(
        ed.git.state.as_ref().and_then(|s| s.sync),
        Some(crate::git::Sync { ahead: 1, behind: 0 }),
        "the watcher picked the counts up"
    );
    std::fs::remove_dir_all(&bare).unwrap();
    for _ in 0..300 {
        ed.poll_git(false);
        if !text(&ed).contains("Staged changes") && !ed.git.in_flight() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!text(&ed).contains("Staged changes"), "{}", text(&ed));
    // c a prefills the last message; :quit cancels.
    keys(&mut ed, "c a");
    assert!(text(&ed).starts_with("local only\n"), "the last message is prefilled: {}", text(&ed));
    keys(&mut ed, "esc : text:quit ret");
    assert!(ed.float.is_none() && ed.message.as_ref().is_some_and(|m| m.text.contains("cancelled")));
    keys(&mut ed, "c c");
    assert!(ed.float.is_none(), "nothing staged: no commit buffer");
    // :help from the status buffer opens in a new tab on the git section.
    keys(&mut ed, ": text:help ret");
    assert_eq!(ed.tabs.len(), 3);
    let line = ed.buffer().line_of(cursor(&ed));
    assert_eq!(ed.buffer().line_text(line), "## Git status buffer");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn welcome_page_lists_recent_entries_and_opens_them() {
    let dir = std::env::temp_dir().join(format!("quin-welcome-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("proj")).unwrap();
    std::fs::write(dir.join("note.txt"), "hello").unwrap();
    let mut ed = editor_with("");
    // Opening a file and changing directory both remember the path.
    keys(&mut ed, &format!(": text:open space text:{} ret", dir.join("note.txt").display()));
    keys(&mut ed, &format!(": text:cd space text:{} ret", dir.join("proj").display()));
    crate::welcome::open(&mut ed);
    assert!(ed.buffer().welcome.is_some() && ed.buffer().readonly);
    let page = text(&ed);
    assert!(page.starts_with("quin\n"), "{page}");
    let line = ed.buffer().line_of(cursor(&ed));
    assert!(ed.buffer().line_text(line).ends_with("proj/"), "the cursor starts on the most recent entry: {page}");
    keys(&mut ed, "j ret");
    assert_eq!(text(&ed), "hello", "ret on a file opens it");
    assert!(ed.buffers.values().all(|b| b.welcome.is_none()), "the welcome buffer is gone");
    crate::welcome::open(&mut ed);
    keys(&mut ed, "ret");
    assert_eq!(ed.cwd, dir.join("proj").canonicalize().unwrap_or(dir.join("proj")), "ret on a directory moves there");
    assert!(ed.buffer().welcome.is_none() && !ed.buffer().readonly && text(&ed).is_empty(), "and the page gives way to a scratch buffer");
    crate::welcome::open(&mut ed);
    keys(&mut ed, "i text:typed esc");
    assert!(ed.buffer().welcome.is_none() && !ed.buffer().readonly);
    assert_eq!(text(&ed), "typed", "typing swaps the page for a scratch buffer");
    // Files picked from inside a directory are not recent entries; only `:open <path>` is.
    let before = crate::welcome::recent();
    keys(&mut ed, &format!(": text:cd space text:{} ret space e text:note ret", dir.display()));
    assert_eq!(text(&ed), "hello");
    let after = crate::welcome::recent();
    assert_eq!(after.iter().filter(|p| p.ends_with("note.txt")).count(), before.iter().filter(|p| p.ends_with("note.txt")).count());
    assert_eq!(after[0], dir.canonicalize().unwrap_or(dir.clone()), "the cd is remembered");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn clipboard_pastes_into_pickers_and_seeds_the_global_search() {
    let dir = std::env::temp_dir().join(format!("quin-paste-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.txt"), "find me(here)\nnot this\n").unwrap();
    let mut ed = editor_with("me(here)\n");
    keys(&mut ed, &format!(": text:cd space text:{} ret", dir.display()));
    // `*` on a selection sets the search pattern; space f searches it everywhere.
    keys(&mut ed, "e *");
    keys(&mut ed, "space f");
    let Overlay::Picker(picker) = &ed.overlay else { panic!("no picker") };
    assert_eq!(picker.query, "me");
    for _ in 0..200 {
        ed.poll_search();
        if ed.live_search.as_ref().is_some_and(|s| s.done) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let Overlay::Picker(picker) = &ed.overlay else { panic!("no picker") };
    assert_eq!(picker.items.len(), 1, "the pattern was found: {:?}", picker.items.iter().map(|i| &i.label).collect::<Vec<_>>());
    // C-S-v pastes the clipboard's first line into a picker and a prompt.
    keys(&mut ed, "esc x y");
    assert_eq!(ed.clipboard.paste().unwrap(), "me(here)\n");
    keys(&mut ed, ": C-S-v");
    let Overlay::Picker(picker) = &ed.overlay else { panic!("no picker") };
    assert_eq!(picker.query, "me(here)");
    keys(&mut ed, "esc / text:x C-S-v");
    let Overlay::Prompt(prompt) = &ed.overlay else { panic!("no prompt") };
    assert_eq!(prompt.text, "xme(here)");
    keys(&mut ed, "esc");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A markdown buffer drawn once into an off-screen frame, ready for mouse
/// hit-testing (which reads the same line layouts as drawing).
fn markdown_editor(text: &str) -> (Editor, crate::render::font::FontSet) {
    let mut ed = editor_with(text);
    let markdown = ed.registry.find("markdown");
    ed.buffer_mut().set_language(markdown);
    let font = crate::render::font::FontSet::builtin(14.0);
    let font = draw_frame(&mut ed, font);
    (ed, font)
}

fn draw_frame(ed: &mut Editor, mut font: crate::render::font::FontSet) -> crate::render::font::FontSet {
    let mut pixels = vec![0u32; 800 * 600];
    let painter = crate::render::Painter::new(&mut pixels, 800, 600);
    let mut ui = crate::render::ui::Ui { painter, font: &mut font };
    crate::render::ui::draw(ed, &mut ui);
    font
}

/// Character index under a pixel of the focused frame.
fn char_at(ed: &mut Editor, font: &crate::render::font::FontSet, x: i32, y: i32) -> usize {
    let (view_id, rect) = ed.frame_rects[0];
    crate::render::ui::position_at(ed, font, view_id, rect, rect.x + x, rect.y + y).unwrap()
}

#[test]
fn markdown_tables_line_up_their_columns() {
    let text = "text\n| a | long cell |\n|---|---|\n| wide one | b |\n";
    let (mut ed, font) = markdown_editor(text);
    let cell_h = font.cell_h;
    // The table (lines 1-3) has half a row of padding above its first row.
    let pad = (cell_h / 2.0).floor();
    let row_y = |line: usize| (cell_h * (line as f32 + 0.5) + if line >= 1 { pad } else { 0.0 }) as i32;
    // Find where "long" starts on the first row, by pixel.
    let long = text.find("long").unwrap();
    let x = (0..800).find(|&x| char_at(&mut ed, &font, x, row_y(1)) == long).expect("long is on screen");
    // In the last row, the same column starts with "b", although "wide one"
    // pushed it further right in the text.
    assert_eq!(char_at(&mut ed, &font, x, row_y(3)), text.find("b |").unwrap());
    // With the cursor on a row, that row shows as written: "long" moves back.
    keys(&mut ed, "j");
    let font = draw_frame(&mut ed, font);
    let raw = (0..800).find(|&x| char_at(&mut ed, &font, x, (cell_h * 1.5) as i32) == long).unwrap();
    assert!(raw < x, "revealed row is not aligned ({raw} vs {x})");
}

#[test]
fn markdown_callouts_end_at_the_first_unquoted_line() {
    let ed = editor_with("");
    let registry = &ed.registry;
    let lang = registry.find("markdown").unwrap();
    let mut highlighter = crate::syntax::highlight::Highlighter::new();
    let mut state = crate::syntax::highlight::LineState::default();
    let mut spans = Vec::new();
    let mut covered = Vec::new();
    for line in ["> [!WARNING]", "> careful", "", "after"] {
        highlighter.highlight_line(registry, lang, line, &mut state, &mut spans);
        covered.push(highlighter.line_regions.len());
    }
    assert_eq!(covered, vec![1, 1, 0, 0], "the blank line closes the callout without being part of it");
    let styles = &ed.styles;
    highlighter.highlight_line(registry, lang, "> [!WARNING]", &mut state, &mut spans);
    let frame = highlighter.line_regions[0];
    let crate::syntax::Rule::Region { scope, .. } = &registry.get(frame.lang).rules[frame.rule] else { panic!("not a region") };
    assert!(styles.lines[scope.0 as usize].bar.is_some(), "callouts have a bar");
    assert_eq!(styles.replacements[spans[1].scope.0 as usize].as_deref(), Some("\u{f421}  Warning"));
}

#[test]
fn markdown_images_show_below_their_line() {
    let icon = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/icon/icon-256.png");
    let text = format!("top\n![logo]({})\nafter\n", icon.display());
    let (mut ed, font) = markdown_editor(&text);
    let after = text.find("after").unwrap();
    // Not loaded yet: the image line is one row of text.
    assert_eq!(char_at(&mut ed, &font, 0, (font.cell_h * 2.5) as i32), after);
    for _ in 0..500 {
        if ed.images.poll() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let font = draw_frame(&mut ed, font);
    // Loaded: the line is the 256px image (its text hidden), "after" comes below.
    let below_top = char_at(&mut ed, &font, 0, (font.cell_h * 2.5) as i32);
    let deep_inside = char_at(&mut ed, &font, 0, (font.cell_h + 200.0) as i32);
    assert_eq!(ed.buffer().line_of(below_top), 1);
    assert_eq!(ed.buffer().line_of(deep_inside), 1);
    assert_eq!(char_at(&mut ed, &font, 0, (font.cell_h * 1.5 + 270.0) as i32), after);
}

#[test]
fn cursor_rides_with_the_text_while_the_view_slides() {
    let text: String = (0..200).map(|i| format!("line {i}\n")).collect();
    let mut ed = editor_with(&text);
    ed.config.scroll_animation = 10_000;
    ed.config.cursor_animation = 10_000;
    let font = crate::render::font::FontSet::builtin(14.0);
    let font = draw_frame(&mut ed, font);
    keys(&mut ed, "C-d");
    assert!(ed.view().anim.is_some(), "the page slides");
    let font = draw_frame(&mut ed, font);
    // The cursor moves with the sliding text instead of chasing it.
    assert!(ed.view().cursor_anim.is_none());
    // Once the slide is over, the cursor slides again for plain moves.
    ed.view_mut().anim = None;
    let font = draw_frame(&mut ed, font);
    keys(&mut ed, "j");
    draw_frame(&mut ed, font);
    assert!(ed.view().cursor_anim.is_some());
}

#[test]
fn touchpad_scrolling_adds_up_small_steps() {
    let mut rest = 0.0;
    // A touchpad swipe: steps of a fifth of a line each scroll every fifth step.
    let lines: Vec<i32> = (0..10).map(|_| crate::wheel_lines(&mut rest, 0.2)).collect();
    assert_eq!(lines.iter().sum::<i32>(), 2, "{lines:?}");
    assert_eq!(lines.iter().filter(|l| **l != 0).count(), 2);
    // A mouse wheel notch still scrolls three lines at once.
    assert_eq!(crate::wheel_lines(&mut rest, 3.0), 3);
    // Turning the other way does not first pay back what was left over.
    let mut rest = 0.0;
    assert_eq!(crate::wheel_lines(&mut rest, 0.9), 0);
    assert_eq!(crate::wheel_lines(&mut rest, -1.2), -1);
}

#[test]
fn welcome_page_lists_ten_entries_and_never_scrolls() {
    let dir = std::env::temp_dir().join(format!("quin-welcome-ten-{}", std::process::id()));
    for i in 0..12 {
        std::fs::create_dir_all(dir.join(format!("d{i}"))).unwrap();
        crate::welcome::remember(&dir.join(format!("d{i}")));
    }
    let mut ed = editor_with("");
    crate::welcome::open(&mut ed);
    let entries = ed.buffer().welcome.as_ref().unwrap().iter().filter(|e| e.is_some()).count();
    assert_eq!(entries, 10);
    // A big font: the page is taller than the 600px test frame, so there is room to scroll.
    let mut font = draw_frame(&mut ed, crate::render::font::FontSet::builtin(40.0));
    for sequence in ["C-d", "C-d C-d", "G", "C-e C-e C-e", "z t", "g g C-f"] {
        keys(&mut ed, sequence);
        font = draw_frame(&mut ed, font);
        let view = ed.view();
        assert_eq!((view.scroll_line, view.scroll_col), (0, 0), "after {sequence}");
        assert!(view.anim.is_none(), "no slide after {sequence}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
