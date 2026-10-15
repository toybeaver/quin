//! Commands typed in the command palette (`:write`, `:quit`, `:open`...).

use std::process::{Command as Process, Stdio};

use crate::commands;
use crate::editor::{Editor, Mode};
use crate::overlays;
use crate::picker::PromptKind;

pub struct TypedCommand {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub doc: &'static str,
    /// The palette keeps the input open with `name ` when chosen without arguments.
    pub needs_arg: bool,
    pub run: fn(&mut Editor, &[String]) -> Result<(), String>,
}

impl std::fmt::Debug for TypedCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TypedCommand({})", self.name)
    }
}

pub fn find(name: &str) -> Option<&'static TypedCommand> {
    COMMANDS.iter().find(|c| c.name == name || c.aliases.contains(&name))
}

pub fn run(ed: &mut Editor, cmd: &'static TypedCommand, args: &[String]) {
    if let Err(err) = (cmd.run)(ed, args) {
        ed.set_error(err);
    }
}

/// Split arguments on whitespace, honouring double quotes.
pub fn split_args(text: &str) -> Vec<String> {
    crate::syntax::format::tokenize(text)
}

// ---------------------------------------------------------------------------

fn write(ed: &mut Editor, args: &[String]) -> Result<(), String> {
    if ed.buffer().is_terminal() {
        return Err("cannot write a terminal buffer".to_string());
    }
    if matches!(ed.mode, Mode::Insert | Mode::Replace) {
        ed.set_mode(Mode::Normal);
    }
    if ed.buffer().tree {
        return crate::tree::write(ed);
    }
    if let Some(amend) = ed.buffer().commit {
        return crate::git::commit_from_buffer(ed, amend);
    }
    let path = args.first().map(|p| crate::files::expand_home(p));
    let path = path.as_deref();
    let buffer = ed.buffer_mut();
    let bytes = buffer.save(path).map_err(|e| e.to_string())?;
    let name = buffer.display_name();
    let lines = buffer.line_count();
    if path.is_some() {
        let buffer_id = ed.view().buffer;
        let registry = &ed.registry;
        if let Some(buffer) = ed.buffers.get_mut(&buffer_id) {
            buffer.detect_language(registry);
        }
    }
    ed.set_message(format!("{name}: wrote {lines} lines, {bytes} bytes"));
    ed.poll_git(true);
    Ok(())
}

fn modified_buffers(ed: &Editor) -> Vec<String> {
    ed.buffers.values().filter(|b| b.is_modified()).map(|b| b.display_name()).collect()
}

/// Close the frame, or quit when it is the last one.
fn quit(ed: &mut Editor, _args: &[String]) -> Result<(), String> {
    if ed.buffer().commit.is_some() {
        crate::git::cancel_commit(ed);
        return Ok(());
    }
    let focused = ed.focused_view();
    if ed.root().leaves().len() > 1 || ed.tabs.len() > 1 {
        ed.close_view(focused);
        return Ok(());
    }
    let modified = modified_buffers(ed);
    if !modified.is_empty() {
        return Err(format!("unsaved changes in {} (use :quit! or :write-quit)", modified.join(", ")));
    }
    ed.quit = true;
    Ok(())
}

fn quit_force(ed: &mut Editor, _args: &[String]) -> Result<(), String> {
    if ed.buffer().commit.is_some() {
        crate::git::cancel_commit(ed);
        return Ok(());
    }
    let focused = ed.focused_view();
    if ed.root().leaves().len() > 1 || ed.tabs.len() > 1 {
        ed.close_view(focused);
        return Ok(());
    }
    ed.quit = true;
    Ok(())
}

fn write_quit(ed: &mut Editor, args: &[String]) -> Result<(), String> {
    // Writing a commit message already closes its frame: nothing else quits.
    let committing = ed.buffer().commit.is_some();
    write(ed, args)?;
    if !committing {
        quit(ed, &[])?;
    }
    Ok(())
}

fn open(ed: &mut Editor, args: &[String]) -> Result<(), String> {
    match args.first() {
        None => {
            overlays::open_file_picker(ed, false);
            Ok(())
        }
        Some(path) => {
            let path = crate::files::expand_home(path);
            if path.is_dir() {
                return change_directory(ed, &[path.to_string_lossy().into_owned()]);
            }
            overlays::open_path(ed, &path, None, crate::picker::OpenMode::Here);
            // Only files opened by name this way count as recent.
            if ed.buffer().path.as_deref() == Some(crate::buffer::absolute(&path).as_path()) {
                crate::welcome::remember(&path);
            }
            Ok(())
        }
    }
}

fn new_buffer(ed: &mut Editor, _args: &[String]) -> Result<(), String> {
    let id = ed.new_scratch_buffer();
    ed.show_buffer(id);
    Ok(())
}

/// `:wrap`: soft-wrap long lines (or not) for the rest of the session.
fn wrap(ed: &mut Editor, _args: &[String]) -> Result<(), String> {
    ed.config.wrap = !ed.config.wrap;
    for view in ed.views.values_mut() {
        view.scroll_col = 0;
    }
    ed.set_message(if ed.config.wrap { "wrap on" } else { "wrap off" });
    Ok(())
}

fn tree(ed: &mut Editor, _args: &[String]) -> Result<(), String> {
    crate::tree::toggle(ed);
    Ok(())
}

/// Open a shell in a new frame below the current one.
/// Open a shell in the current frame, like any other buffer.
fn terminal(ed: &mut Editor, _args: &[String]) -> Result<(), String> {
    let id = ed.new_terminal_buffer()?;
    ed.show_buffer(id);
    ed.set_mode(Mode::Terminal);
    Ok(())
}

/// A bundled theme, or one from the user's `themes/` directory.
pub fn load_theme(name: &str) -> Option<crate::theme::Theme> {
    crate::theme::Theme::builtin(name).or_else(|| {
        crate::config::config_dir()
            .map(|d| d.join("themes").join(format!("{name}.theme")))
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|src| crate::theme::Theme::parse(name, &src))
    })
}

pub fn set_theme(ed: &mut Editor, name: &str) {
    match load_theme(name) {
        Some(theme) => {
            ed.set_theme(theme);
            ed.config.theme = name.to_string();
            ed.set_message(format!("theme: {name}"));
        }
        None => ed.set_error(format!("unknown theme `{name}`")),
    }
}

fn theme(ed: &mut Editor, args: &[String]) -> Result<(), String> {
    match args.first() {
        Some(name) => set_theme(ed, name),
        None => overlays::open_theme_picker(ed),
    }
    Ok(())
}

pub fn set_language(ed: &mut Editor, name: &str) {
    if name.is_empty() || name == "plain text" || name == "text" {
        ed.buffer_mut().set_language(None);
        ed.set_message("language: plain text");
        return;
    }
    match ed.registry.find(name) {
        Some(id) => {
            ed.buffer_mut().set_language(Some(id));
            ed.set_message(format!("language: {}", ed.registry.get(id).name));
        }
        None => ed.set_error(format!("unknown language `{name}`")),
    }
}

fn language(ed: &mut Editor, args: &[String]) -> Result<(), String> {
    match args.first() {
        Some(name) => set_language(ed, name),
        None => overlays::open_language_picker(ed),
    }
    Ok(())
}

fn reload(ed: &mut Editor, _args: &[String]) -> Result<(), String> {
    let registry = &ed.registry;
    let (view, buffer) = {
        let view_id = ed.focused_view();
        let view = ed.views.get_mut(&view_id).expect("view");
        let buffer = ed.buffers.get_mut(&view.buffer).expect("buffer");
        (view, buffer)
    };
    let Some(path) = buffer.path.clone() else { return Err("buffer has no file".to_string()) };
    let fresh = crate::buffer::Buffer::open(buffer.id, &path, registry).map_err(|e| e.to_string())?;
    *buffer = fresh;
    view.selection = view.selection.clamp(buffer.len_chars());
    ed.set_message("reloaded");
    Ok(())
}

fn change_directory(ed: &mut Editor, args: &[String]) -> Result<(), String> {
    let target = match args.first() {
        Some(dir) => crate::buffer::absolute(&crate::files::expand_home(dir)),
        None => std::env::var_os("HOME").map(std::path::PathBuf::from).ok_or("no HOME")?,
    };
    cd(ed, &target)
}

/// Make `target` the working directory (`:cd`, the welcome page, `:open` on
/// a directory). Leaving the welcome page this way lands in a scratch buffer.
pub fn cd(ed: &mut Editor, target: &std::path::Path) -> Result<(), String> {
    std::env::set_current_dir(target).map_err(|e| format!("{}: {e}", target.display()))?;
    ed.cwd = target.to_path_buf();
    ed.file_list = None;
    crate::welcome::remember(target);
    // The welcome page belongs to the directory it was opened in: every
    // frame showing it gets a scratch buffer instead.
    let welcomes: Vec<crate::buffer::BufferId> = ed.buffers.values().filter(|b| b.welcome.is_some()).map(|b| b.id).collect();
    for welcome in welcomes {
        let scratch = ed.new_scratch_buffer();
        for view in ed.views.values_mut().filter(|v| v.buffer == welcome) {
            view.switch_buffer(scratch);
        }
        let _ = ed.close_buffer(welcome, true);
    }
    // The tree follows the working directory.
    if ed.tree.is_some() {
        crate::tree::follow_cwd(ed);
    }
    ed.set_message(format!("cwd: {}", target.display()));
    Ok(())
}

fn pwd(ed: &mut Editor, _args: &[String]) -> Result<(), String> {
    ed.set_message(ed.cwd.display().to_string());
    Ok(())
}

fn shell_command(command: &str) -> Result<String, String> {
    let (shell, flag) = if cfg!(windows) { ("cmd", "/C") } else { ("sh", "-c") };
    let output = Process::new(shell)
        .arg(flag)
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("cannot run {shell}: {e}"))?;
    if !output.status.success() && output.stdout.is_empty() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Run a shell command and show its output in a message.
pub fn run_shell(ed: &mut Editor, command: &str) {
    match shell_command(command) {
        Ok(out) => ed.set_message(out.trim_end().replace('\n', " | ")),
        Err(err) => ed.set_error(err),
    }
}

fn sh(ed: &mut Editor, args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        overlays::open_prompt(ed, PromptKind::Shell, "sh:");
        return Ok(());
    }
    run_shell(ed, &args.join(" "));
    Ok(())
}

const HELP_TREE: &str = "\
The file tree (`A-e`, `:tree`) is a sidebar buffer listing the working
directory, in the spirit of oil.nvim. Files open in the frame the tree was
opened from; the tree stays. Editing the text and then `:write` changes the
file system: a changed name renames, a new line creates (a trailing `/` makes
a directory, two spaces of indentation per level nest it), a line moved under
another directory moves the entry, a removed line deletes it. `o` on an
unfolded directory starts the line inside it. Deletes are announced first and
applied by a second `:write`. Dot-files start hidden;
`.` shows and hides them. Git marks: changed files orange, new files green,
`* ` before directories with changes inside.

";

const HELP_GIT: &str = "\
The git status buffer (`space l`) lists untracked, unstaged and staged files.
`ret` opens the file in a read-only preview split on the right, coloured
against HEAD; `tab` unfolds its diff; `s` stages the file or hunk under the
cursor (or unstages it in the staged section); `c c` / `c a` open a floating
commit message: `:write` commits, `:quit` cancels; `p` pulls and `P` pushes.

";

/// `:help`: every binding and command in a buffer. From a special buffer
/// (tree, git, terminal) it opens in a new tab on that buffer's section.
fn help(ed: &mut Editor, _args: &[String]) -> Result<(), String> {
    let mut text = String::from("# quin keybindings\n\n");
    let maps = [
        ("Normal mode", &ed.keymaps.normal, ""),
        ("Insert and replace mode", &ed.keymaps.insert, ""),
        ("Terminal mode", &ed.keymaps.terminal, ""),
        ("File tree", &ed.keymaps.tree, HELP_TREE),
        ("Git status buffer", &ed.keymaps.git, HELP_GIT),
    ];
    for (title, map, intro) in maps {
        text.push_str(&format!("## {title}\n\n{intro}"));
        let mut bindings = map.bindings();
        bindings.sort_by(|a, b| a.1.name.cmp(b.1.name).then(a.0.cmp(&b.0)));
        for (keys, cmd) in bindings {
            // Backticks keep markdown from reading a key like `-` as a list marker.
            let keys = format!("`{keys}`");
            text.push_str(&format!("{keys:<18} {:<36} {}\n", cmd.name, cmd.doc));
        }
        text.push('\n');
    }
    text.push_str("## Commands (:)\n\n");
    for cmd in COMMANDS {
        let aliases = if cmd.aliases.is_empty() { String::new() } else { format!(" ({})", cmd.aliases.join(", ")) };
        text.push_str(&format!("{:<24} {}{aliases}\n", cmd.name, cmd.doc));
    }
    let current = ed.buffer();
    let section = if current.tree {
        Some("## File tree")
    } else if current.git || current.commit.is_some() {
        Some("## Git status buffer")
    } else if current.is_terminal() {
        Some("## Terminal mode")
    } else {
        None
    };
    let id = ed.new_scratch_buffer();
    let buffer = ed.buffers.get_mut(&id).expect("new buffer");
    buffer.set_text(&text);
    buffer.set_language(ed.registry.find("markdown"));
    buffer.readonly = true;
    match section {
        Some(heading) => {
            ed.tab_new(Some(id));
            if let Some(line) = text.lines().position(|l| l == heading) {
                commands::goto_line_number(ed, line, false);
                let view = ed.view_mut();
                view.scroll_line = line;
                view.anim = None;
            }
        }
        None => ed.show_buffer(id),
    }
    Ok(())
}

fn config(ed: &mut Editor, _args: &[String]) -> Result<(), String> {
    let path = crate::config::config_file().ok_or("cannot find the config directory")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    overlays::open_path(ed, &path, None, crate::picker::OpenMode::Here);
    if ed.buffer().text.len_chars() == 0 {
        let template = ed.config.template(&ed.font_family);
        ed.buffer_mut().set_text(&template);
    }
    Ok(())
}

macro_rules! typed_commands {
    ($($name:literal, [$($alias:literal),*], $doc:literal, $needs_arg:literal => $f:ident;)*) => {
        pub static COMMANDS: &[TypedCommand] = &[
            $(TypedCommand { name: $name, aliases: &[$($alias),*], doc: $doc, needs_arg: $needs_arg, run: $f }),*
        ];
    };
}

typed_commands! {
    "write", ["w"], "Write the buffer to its file (or to <path>)", false => write;
    "write-quit", ["wq", "x"], "Write, then close the frame or quit", false => write_quit;
    "quit", ["q"], "Close the frame, or quit when it is the last one", false => quit;
    "quit!", ["q!"], "Close the frame or quit, discarding changes", false => quit_force;
    "open", ["o", "e"], "Open a file (<path>), or the file picker", false => open;
    "new", ["n"], "New scratch buffer", false => new_buffer;
    "terminal", ["term"], "Open a terminal in this frame", false => terminal;
    "tree", [], "Show or hide the file tree sidebar", false => tree;
    "wrap", [], "Toggle soft wrap of long lines", false => wrap;
    "theme", [], "Switch theme (<name>, or pick one)", false => theme;
    "language", ["lang"], "Set the buffer's language (<name>, or pick one)", false => language;
    "reload", ["rl"], "Reload the file from disk", false => reload;
    "cd", [], "Change the working directory", true => change_directory;
    "pwd", [], "Show the working directory", false => pwd;
    "sh", ["!"], "Run a shell command and show its output", false => sh;
    "help", [], "Show all keybindings and commands", false => help;
    "config", [], "Open the configuration file (config.toml)", false => config;
}
