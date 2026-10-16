# AGENTS.md

Instructions for anyone (human or agent) working on quin. They come from the
project owner and are not negotiable unless the owner changes them.

## Goal

A plain, code-oriented text editor with a native GUI. Think "Helix in a
window, with buffers like neovim". Nothing more.

- Runs on Linux, macOS and Windows. Keep a WebAssembly target in mind: keep
  platform-specific code (pty, clipboard, file system, window) in the few files
  that already hold it (`terminal.rs`, `clipboard.rs`, `files.rs`, `main.rs`).
- Fast above all. The reference machine is a Raspberry Pi 4 running Raspbian;
  the editor must not stutter there. No GPU, no busy loops, nothing that
  re-highlights or re-lays-out a whole file on every keystroke.
- Opened on a file or a directory. Not project oriented: no workspaces, no
  project files.

## Hard constraints

- **Native rendering only.** No web stack (no Tauri, Leptos, Electron, HTML
  views). The window is `winit`, pixels go through `softbuffer`, text is
  rasterized with `fontdue`.
- **No LSP, no AI, no plugins, nothing fancy.** Syntax highlighting is the
  ceiling, and it uses the editor's own `.syn` format (see `README.md` and
  `src/syntax/format.rs`). Converters from other formats (TextMate, tree-sitter
  `highlights.scm`) live in `src/syntax/convert.rs`.
- **Few, light dependencies.** Every new crate needs a reason that the
  existing ones cannot cover. Do not add a dependency for convenience. Current
  set: winit, softbuffer, fontdue, rustybuzz (ligature shaping only), ropey,
  regex, unicode-width, vte, portable-pty.
- **Readable, simple code.** The owner reads and edits this code by hand.
  Plain functions, few abstractions, no clever generics or trait towers, but
  do not avoid an abstraction that genuinely removes repetition. Every file
  starts with a comment saying what it is for. Prefer a longer obvious
  function over a short cryptic one.
- Work only inside this repository. Do not touch other directories on the
  machine, do not install things globally.

## Editing model and UI rules

- Configuration is `config.toml` in the OS config directory (flat TOML,
  parsed by `config.rs`; no TOML crate). Themes: `dark`, `light`, plus the
  bundled iTerm2-Color-Schemes collection derived from terminal palettes in
  `theme.rs`.
- Keybindings follow Helix, trimmed to the basics. The tables in
  `src/keymap.rs` are the source of truth; the README mirrors them. When you
  change a binding, change both.
- `:` opens a fuzzy command palette (like Zed or VS Code), not a vim-style
  command line. Keep the list of `:` commands short: write, write-quit,
  quit, quit!, open, new, terminal, tree, wrap, theme, language, reload, cd,
  pwd, sh, help, config.
- Buffers are the unit, like neovim: files, scratch buffers and terminals are
  all buffers. A terminal is an ordinary buffer with an extra "terminal" mode
  that forwards keys to the shell. There is no special terminal frame, no
  bottom-panel concept; `:terminal` just splits the current frame.
- Frames (splits) and tabs are driven from `space w`. The main layout is just
  the text plus one status line at the bottom; a tab bar appears only when
  there is more than one tab.
- Yank, delete and paste always use the system clipboard. There are no
  registers.
- The editor is not project bound: the file picker starts from the working
  directory but browses any path typed into it. Global search streams results
  from a background thread; never block the UI on a search.
- Fonts: Iosevka (Nerd Font Mono build, regular/bold/italic/bold-italic,
  subset with fonttools to the scripts and symbols the editor needs) and the
  Nerd Font symbol set are embedded;
  `font-family`/`font` in the config swap in a system font. Support
  ligatures. Do not grow the embedded fonts beyond a few megabytes.
- Pickers that open files (files, buffers, search results) always show a
  preview pane of the highlighted item; the theme picker previews on the
  whole window and rolls back on cancel.
- Frame resizing moves only the edge shared with the adjacent frame;
  splitting, closing a frame or `space w 0` gives every frame an equal share.
- Scrolling and the cursor animate (`View::scroll_to`, `View::cursor_position`);
  keep animations short, cheap and configurable, and never let them block
  input.
- Only input redraws at once. Animation frames and redraws for background
  work (terminal output, search results, git) are paced to one per
  `FRAME_INTERVAL` (`main.rs`). Never request a redraw in a loop. Work done
  on every frame must stay bounded by what is on screen (see
  `matching_bracket_within`).
- The file tree (`A-e`, `:tree`) is a sidebar buffer in the spirit of
  oil.nvim: `ret` opens or folds, and `:write` turns text edits into renames,
  creates, moves and deletes (deletes need a second `:write`). Keep it a
  buffer; do not turn it into a widget with its own key handling.
- Git integration stays small and reads from the `git` binary on a
  background thread (no libgit2-style crate): branch badge in the status
  line, file and directory markers in the tree, and a neogit-like status
  buffer (`space l`, its own tab): `ret` opens files in a reused read-only
  preview split on the right (coloured against HEAD), `tab` unfolds a file's
  diff, `s` stages or unstages files and hunks (`git add`,
  `git restore --staged`, `git apply --cached`), `esc` closes the preview,
  `c c` / `c a` commit or amend through a floating commit-message frame
  (`Editor::float`; `:write` commits with `git commit -F -`, `:quit` cancels),
  `p` / `P` pull and push on a background thread (`git::run_remote`), and a
  `git fetch` every 10 minutes (and after each git action) on its own thread
  feeds an ahead/behind block in the status line, shown only when non-zero.
  `space l` in the status buffer forces a refresh. Every file gets git
  signs on the left edge of its frame (`signs.rs`): its text, unsaved edits
  included, is diffed against its HEAD version on a background thread after
  each edit; the HEAD version is read again when HEAD or the status changes.
  Nothing git-related may ever block or break the editor: every
  call runs off the UI thread with prompts disabled, and failures just mean
  stale information.
  `space` stays the leader key everywhere. The git buffer's keys live in
  `GIT_KEYS` and are tried before the normal keymap.
- Default font: the embedded Iosevka.
- Starting without a file shows the welcome buffer (`welcome.rs`): logo,
  greeting, ten recent entries kept in `<config dir>/recent`; `ret` opens,
  insert swaps it for a scratch buffer. Keep it a plain buffer. It never
  scrolls (`draw_view` pins it to the top, the wheel is ignored).
- `p`/`P` paste like vim. In normal mode they insert after/before the
  selection without replacing it (whole lines go below/above the line). In
  select mode they replace the selection like vim's visual-mode paste: `p`
  puts the replaced text on the clipboard, `P` does not. `r` replaces the
  character under the cursor; `R` enters replace mode.
- Syntax files may also say how to *render* a token (bigger headings,
  concealed markers, replaced bullets, embedded languages) or a block of
  lines (`bar=`, `center`, `grid` tables, `image`), which is how Markdown
  gets a light "rendered" look like Emacs' markdown-mode: tables, callouts,
  images and inline HTML included. Line-level rendering lives in
  `render/blocks.rs`; images are PNG only, decoded by the small decoder in
  `render/png.rs` and loaded off the UI thread by `images.rs`.

## How to verify changes

```sh
cargo build --release          # the binary should stay small (~6 MB) and start instantly
cargo test                     # unit tests + key-replay end-to-end tests in src/tests.rs
cargo clippy --all-targets     # must be warning-free
cargo fmt                      # rustfmt.toml: max_width 140
./target/release/quin --screenshot out.ppm --keys "space e" README.md   # headless render
```

The `--screenshot` mode renders one frame without a window; use it to check
drawing changes. Add an end-to-end test in `src/tests.rs` for every behaviour
change: they replay key sequences through the real dispatcher.

## When done

Update `README.md` (features, keybinding tables, commands, limitations) so it
matches the code, and keep this file current if a rule changes.
