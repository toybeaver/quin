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
    assert_eq!(text(&ed), "one\none\n", "linewise paste replaces the whole line under the cursor");
    assert_eq!(ed.clipboard.paste().unwrap(), "zero\n", "p yanks what it replaced");
    keys(&mut ed, "j x P");
    assert_eq!(text(&ed), "one\nzero\n");
    assert_eq!(ed.clipboard.paste().unwrap(), "zero\n", "P leaves the clipboard alone");
}

#[test]
fn paste_replaces_the_selection() {
    let mut ed = editor_with("alpha beta\n");
    keys(&mut ed, "e y / text:beta ret p");
    assert_eq!(text(&ed), "alpha alpha\n");
    assert_eq!(ed.clipboard.paste().unwrap(), "beta", "the replaced word is now on the clipboard");
    keys(&mut ed, "g h e p");
    assert_eq!(text(&ed), "beta alpha\n", "p again swaps them back");
    keys(&mut ed, "g h e P");
    assert_eq!(text(&ed), "alpha alpha\n");
    assert_eq!(ed.clipboard.paste().unwrap(), "alpha");
}

#[test]
fn yank_with_several_cursors_pastes_one_piece_per_cursor() {
    let mut ed = editor_with("a1\nb2\nc3\n");
    keys(&mut ed, "C C e y");
    assert_eq!(ed.clipboard.paste().unwrap(), "a1\nb2\nc3");
    keys(&mut ed, ", g g l P");
    assert_eq!(text(&ed), "aa1\nb2\nc3\nb2\nc3\n", "a single cursor pastes the whole clipboard");
    keys(&mut ed, "u g g l C C P");
    assert_eq!(text(&ed), "aa1\nbb2\ncc3\n", "three cursors get their own piece again");
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
    assert_eq!(ed.root().leaves().len(), 2, ":terminal splits the frame");
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
    keys(&mut ed, "C-= C-+ C--");
    assert_eq!(ed.config.font_size, 15.0);
    assert!(ed.font_dirty);
    keys(&mut ed, "i C--");
    assert_eq!(ed.config.font_size, 14.0, "zoom works in insert mode too");
}

#[test]
fn tree_sidebar_opens_folds_and_edits_files() {
    let dir = std::env::temp_dir().join(format!("quin-tree-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/lib.rs"), "lib").unwrap();
    std::fs::write(dir.join("notes.md"), "notes").unwrap();
    let mut ed = editor_with("");
    keys(&mut ed, &format!(": text:cd space text:{} ret A-e", dir.display()));
    assert_eq!(ed.root().leaves().len(), 2);
    assert!(ed.buffer().tree);
    assert_eq!(text(&ed), "src/\nnotes.md\n");
    keys(&mut ed, "ret");
    assert_eq!(text(&ed), "src/\n  lib.rs\nnotes.md\n", "ret on a directory expands it");
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
        ed.root().layout(crate::render::Rect::new(0, 0, 1001, 501), &mut rects);
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
    ed.root().layout(crate::render::Rect::new(0, 0, 1001, 501), &mut rects);
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
        ed.root().layout(crate::render::Rect::new(0, 0, 1000 + expected.len() as i32 - 1, 500), &mut rects);
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
    assert_eq!(ed.config.theme, "dark");
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
