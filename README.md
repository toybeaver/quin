# quin

> [!WARNING]
> Although this text editor targets fully manual (artisanal) coding, it is COMPLETELY vibe-coded. 
> I do intend to rewrite parts of it by hand, but this will forever be an AI-centric codebase due to 2 reasons:
> 
> 1. I really wanted a code editor thats plug and play for my necessities without actually requiring to tweak keybindings, plugins, etc; and
> 2. I have many more projects that are more interesting to me than a text editor.
> 
> Feel free to open PRs or code suggestions though!

A small, fast, keyboard-driven code editor with a native GUI. No LSP, no AI,
no plugins: text, syntax highlighting, splits, tabs, fuzzy pickers and a real
terminal in a buffer. Helix-style modal editing, trimmed to the basics.

- **Native and light.** Rust, software rendering (`winit` + `softbuffer`),
  nine direct dependencies, a ~9 MB binary, no GPU needed. Runs on Linux
  (Wayland/X11), macOS and Windows. Uses the Nerd Font installed on your
  system (Iosevka preferred) with ligatures; Nerd Font symbols are embedded
  so icons always render.
- **Buffers and frames.** Buffers are files, scratch pads or terminals. Frames
  split the window (side by side or stacked); tabs hold layouts. Everything is
  driven from the keyboard.
- **Own syntax format.** Highlighting rules live in small readable `*.syn`
  files which also say *how* tokens render: Markdown headings are bigger,
  `**markers**` are hidden, list bullets become `•`, fenced code is highlighted
  in its own language. Converters import TextMate grammars and tree-sitter
  `highlights.scm` files.
- **Fuzzy everything.** File picker (any directory on the machine), buffer
  picker, command palette, and a global search that fills in as you type.

`AGENTS.md` lists the goals and constraints the project is built under.

## Build and run

```sh
cargo build --release
./target/release/quin                 # scratch buffer, cwd = current directory
./target/release/quin src/main.rs     # open a file
./target/release/quin ~/project       # a directory sets the working directory
./target/release/quin a.rs b.rs       # several files (buffers)
```

Linux needs the usual `libxkbcommon` / Wayland or X11 client libraries at run
time (they are loaded dynamically). The system clipboard uses `wl-copy`,
`xclip`, `xsel`, `pbcopy` or `clip`, whichever exists; without them an
in-process clipboard is used.

Everything is also reachable without a window: `quin --screenshot out.ppm
--keys "space e" file.rs` renders one frame to an image, which is how the UI is
tested (`wait` in the key list lets a shell or a search finish).

## The basics

quin is modal, like Helix (and Kakoune): **selection first, then action**.
Every cursor is a one-character selection; motions extend or replace the
selection and `d`, `c`, `y` act on it. Yank, delete and paste always go
through the system clipboard; there are no registers.

| Mode | How to get there | What keys do |
|---|---|---|
| **NOR** normal | `esc` | motions, selections, commands |
| **INS** insert | `i a I A o O c` | type text; `esc` returns to normal |
| **REP** replace | `R` | like insert, but typing overwrites |
| **SEL** select | `v` | like normal, but motions extend the selection |
| **TRM** terminal | `:terminal`, or `i` on a terminal buffer | keys go to the shell; `C-esc` for normal mode |

`:` opens the **command palette**: start typing to fuzzy-filter commands,
`ret` runs the highlighted one, `tab` completes its name. Commands with
arguments are typed in full (`:write notes.md`, `:cd ~/src`). While the
argument of `cd`, `open` or `write` is typed, the list shows the entries of
the directory typed so far: `tab` completes the highlighted one, `ret` runs
the command with it (`open` descends into directories first). Typing a number
(`:42`) jumps to that line.

Scroll jumps (`C-d`, `C-u`, search hits, `gg`/`ge`) slide to their target
over 120 ms, and the cursor slides to its new place over 80 ms, a quieter
take on Neovide's cursor. `scroll-animation` and `cursor-animation` in the
config change or disable them.

The status bar at the bottom shows the mode, the buffer name (`[+]` when
modified), messages, pending keys, selection count, `line:col` and language.
A tab bar appears at the top only when there is more than one tab.

### Keybindings

Notation: `C-` control, `A-` alt, `space` the space bar. A count typed before
a key repeats it (`3w`, `10j`). `:help` opens the same list inside the editor.

#### Movement

| Key | Action |
|---|---|
| `h` `j` `k` `l` | move left/down/up/right |
| `w` `b` `e` | select to next word start / previous word start / next word end |
| `W` `B` `E` | same with WORDs (whitespace-separated) |
| `f` `t` `F` `T` + char | select to / till the next (previous) occurrence of a character |
| `C-d` `C-u` | half page down / up |
| `C-o` `C-i` | jump backward / forward in the jump list |
| `%` | select the whole buffer |
| `C-=` `C-+` / `C--` | bigger / smaller font (any mode) |
| `ret` | in the tree sidebar: open the file or fold the directory |
| `gg` / `ge` | file start / file end (`3gg` goes to line 3) |
| `gh` `gl` `gs` | line start / last character / first non-blank character |
| `gn` `gp` | next / previous buffer |
| `g.` | position of the last change |
| `gf` | open the file whose name is under the cursor |
| `gt` `gT` | next / previous tab |
| `mm` | matching bracket; from inside a `()` `[]` `{}` pair, its closing bracket |
| `mi` + object | select inside: `w` word, `W` WORD, `p` paragraph, `(` `[` `{` `<` `"` `'` `` ` ``, `m` closest pair |
| `ma` + object | select around (delimiters included) |

#### Selections

| Key | Action |
|---|---|
| `x` | select the current line; again extends to the next line |
| `X` | the same, growing upwards |
| `;` | collapse selections to cursors |
| `A-;` | flip anchor and cursor |
| `,` | keep only the primary selection |
| `C` / `A-C` | add a cursor on the next / previous line |
| `s` | select every regex match inside the selections |
| `v` | toggle select mode |
| `esc` | back to normal mode |

#### Changes

| Key | Action |
|---|---|
| `i` `a` | insert before / after the selection |
| `I` `A` | insert at the line start / end |
| `o` `O` | open a line below / above |
| `d` / `A-d` | delete the selection (to the clipboard / without yanking) |
| `c` / `A-c` | change the selection (delete and insert / without yanking) |
| `y` | yank the selection to the clipboard |
| `p` / `P` | replace the selection with the clipboard; `p` puts the replaced text on the clipboard, `P` keeps the clipboard |
| `r` + char | replace the character under the cursor |
| `R` | replace mode: typing overwrites until `esc` |
| `>` `<` | indent / unindent |
| `u` `U` | undo / redo (insert-mode typing undoes in one step) |
| `C-c` | comment / uncomment the selected lines |
| `.` | repeat the last insertion |

Paste follows vim's visual-mode rules: the clipboard replaces whatever is
selected, and whole lines on the clipboard (`x y`, then `p`) replace whole
lines. With several cursors, a yank made with the same number of cursors
pastes one piece per cursor.

#### Search

| Key | Action |
|---|---|
| `/` `?` | search forward / backward (regex, incremental, smart case) |
| `n` `N` | next / previous match |
| `*` | use the selection as the search pattern |
| `space f` | search a regex in every file of the working directory (live results) |

#### Space menu

| Key | Action |
|---|---|
| `space e` | file picker (working directory, honours `.gitignore`, hides dot files) |
| `space E` | file picker with every file, ignored and hidden ones included |
| `space b` | buffer picker |
| `space f` | global search |
| `space c` | close the frame |
| `A-e` | show / hide the file tree sidebar |
| `space w ...` | window menu (below) |

Inside pickers: type to filter, `up`/`down` or `C-p`/`C-n` to move, `ret` to
open, `C-v` / `C-s` to open in a vertical / horizontal split, `tab` to
complete, `esc` to close. File, buffer and search pickers show a preview of
the highlighted item on the right: the start of the file, or, for search
results, the matching line highlighted in the middle. The mouse wheel scrolls
the preview, or the list when the pointer is over it.

The file picker is not bound to the working directory: type a path starting
with `/`, `~/`, `./` or `../` and it lists that directory instead (directories
first, `tab` or `ret` descends, `../` goes up). A path that does not exist yet
opens as a new file.

Global search runs in the background: results stream into the picker as you
type (at most 2000), with the match highlighted. `ret` jumps to the line.

#### Frames and tabs (`space w`)

| Key | Action |
|---|---|
| `w` / `W` | focus the next / previous frame |
| `v` / `s` | split vertically (side by side) / horizontally (stacked) |
| `n` / `N` | vertical / horizontal split with a new scratch buffer |
| `h` `j` `k` `l` | focus the frame to the left / below / above / right |
| `H` `J` `K` `L` | swap with that frame |
| `q` | close the frame |
| `o` | close every other frame |
| `t` / `x` | new tab / close tab |
| `+` / `-` | taller / shorter, taking the space from the frame next to it (the menu stays open: keep pressing, or hold the key) |
| `>` / `<` | wider / narrower (same) |
| `0` | every frame of the tab back to an equal share (splitting or closing a frame does this too) |

#### File tree

`A-e` or `:tree` toggles a sidebar on the right listing the working
directory. It is a buffer like any other, in the spirit of oil.nvim:

- `ret` on a directory folds or unfolds it; `ret` on a file opens it in the
  frame the tree was opened from (the tree stays open).
- Edit the text to change the file system, then `:write`: a changed name
  renames, a new line creates (a trailing `/` makes a directory, indentation
  of two spaces per level puts it inside the directory above), a line moved
  under another directory moves the entry, a removed line deletes it. Deletes
  are announced first and applied by a second `:write`. When several names in
  one directory change at once and they do not resemble each other, quin
  refuses rather than guess; rename those one at a time.

#### Insert and replace mode

| Key | Action |
|---|---|
| `esc` | normal mode |
| `ret` | newline, keeping the indentation (and indenting after `{`, `(`, `[`) |
| `tab` | insert an indent |
| `backspace` / `del` | delete before / under the cursor (an empty pair `()` goes at once, leading spaces by indent level) |

Brackets and quotes auto-pair (`auto-pairs off` in the config disables it).

#### Terminal mode

`:terminal` splits the current frame and runs your shell (`$SHELL`, or
`shell` in the config) in the new one. A terminal is an ordinary buffer: in
terminal mode keys go straight to the shell except `C-esc` (normal mode),
`C-S-v` (paste the clipboard into the shell) and `C-S-c` (copy the
selection). In normal mode the screen and scrollback are plain text you can
move through, search and yank with the usual keys; the view starts at the top
of the screen, `k` scrolls up into the scrollback. `p` in normal mode types
the clipboard into the shell. `i` or `a` go back to terminal mode. When the
shell exits, any key closes the buffer.

#### Mouse

Click to place the cursor (shift-click extends), drag to select, wheel to
scroll, click a tab to switch to it.

### Commands (`:`)

| Command | Aliases | Action |
|---|---|---|
| `write [path]` | `w` | write the buffer (to `path` if given) |
| `write-quit` | `wq` `x` | write, then close the frame or quit |
| `quit` / `quit!` | `q` `q!` | close the frame, or quit when it is the last one (`!` discards changes) |
| `open [path]` | `o` `e` | open a file, or the file picker |
| `new` | `n` | new scratch buffer |
| `terminal` | `term` | open a terminal in a frame below |
| `tree` | | show or hide the file tree sidebar |
| `theme [name]` | | switch theme; without a name the picker previews each theme on the whole window as you move through it (`esc` goes back) |
| `language [name]` | `lang` | set the buffer's language (picker without a name) |
| `reload` | `rl` | reload the file from disk |
| `cd [dir]` / `pwd` | | working directory |
| `sh <cmd>` | `!` | run a command, show its output |
| `help` | | every binding and command, in a buffer |
| `config` | | open the configuration file |

Closing the window with unsaved changes asks you to close a second time.

## Configuration

`:config` opens the configuration file, creating it with the current font
size, font family and theme. It lives in the OS configuration directory:

| OS | File |
|---|---|
| Linux, BSD | `~/.config/quin/config.toml` (`$XDG_CONFIG_HOME` honoured) |
| macOS | `~/Library/Application Support/quin/config.toml` |
| Windows | `%APPDATA%\quin\config.toml` |

`QUIN_CONFIG_DIR` overrides the directory. The file is flat TOML, one
`key = value` per line:

```toml
font-size    = 14
font-family  = "Iosevka Nerd Font"   # any installed font; default: the first Nerd Font found
font         = "~/fonts/mono.ttf"    # a font file instead of a family; bold-font too
ligatures    = true
theme        = "dark"                # dark, light, a bundled scheme, or a file in themes/
tab-width    = 4
line-numbers = "on"                  # "on", "off", "relative"
cursorline   = true
scrolloff    = 3
auto-pairs   = true
scroll-animation = 120               # milliseconds a scroll jump slides; 0 jumps at once
cursor-animation = 80                # milliseconds the cursor slides; 0 jumps at once
shell        = "/bin/zsh"
```

**Themes.** `dark` and `light` are quin's own. The other 800-odd names in
`:theme` are the [iTerm2-Color-Schemes](https://github.com/mbadolato/iTerm2-Color-Schemes)
collection that Ghostty ships (`catppuccin-mocha`, `gruvbox-dark`,
`solarized-light`...): each 16-color terminal palette is turned into a full
editor theme, and terminals inside quin use that palette as is.

**Fonts.** `font-family` names an installed font by its family name, matched
against the font file names without spaces or case (`"JetBrainsMono Nerd
Font"`, `"Iosevka Nerd Font"`). Without it quin looks for a Nerd Font in the
usual font directories, in this order: Iosevka, Iosevka Term, JetBrains Mono,
Fira Code, Hack, then any other `*NerdFont*-Regular.ttf`. Install
[Iosevka Nerd Font](https://www.nerdfonts.com/font-downloads) to get the
intended look. When none is installed the embedded Hack is used. In every
case the Nerd Font symbol set is embedded as a fallback, so prompt and file
icons render. Ligatures (`->`, `!=`, `::`...) come from the font's own rules
(`ligatures off` disables them).

The same directory can hold:

- `syntax/*.syn` — extra languages (a file with the same `name` as a built-in
  replaces it);
- `themes/*.theme` — extra themes (`scope #rrggbb [bold] [italic]`, see
  `assets/themes/dark.theme` for every scope).

## Syntax files

Languages are described in `assets/syntax/*.syn` (built into the binary) with
a tiny line-oriented format. Arguments are separated by spaces; quote with
`'...'` or `"..."` when a regex contains spaces (no escapes inside quotes).

```
name          rust
extensions    rs
line-comment  //
block-comment /* */
indent        4 spaces
auto-pairs    () [] {} "" ``

keywords keyword.control  if else match loop while for in break continue return
keywords type.builtin     bool char str u8 u16 u32 String Vec Option Result

match comment          //.*
match string           "(?:[^"\\]|\\.)*"
  match string.escape  \\.                      # indented: applies inside the rule above
match function         \b([A-Za-z_]\w*)\s*\(   1:function   # capture 1 gets its own scope
match number           \b\d[\d_]*\b

region comment         /\*  \*/                 # multi-line, may span lines
region code.block      '^```\s*(\w*)'  '^```'  embed=1   # embed the language named by capture 1

style comment.todo bold
```

Rules are tried in order at each position; the earliest match wins (first
rule on ties). Regexes use Rust's `regex` syntax: no look-around, no
back-references. `^`/`$` are line boundaries. A region's end may be `$`, which
makes a one-line region that can hold inner rules.

**Rendering** is where the format differs from classic highlighters.
`style <scope> ...` attaches display attributes to a scope:

| Attribute | Effect |
|---|---|
| `bold` `italic` `underline` `strike` | text style |
| `size=1.5` | bigger text (headings) |
| `conceal` | hidden unless the cursor is on the line |
| `replace=•` | draw this text instead of the token |
| `bg=<scope>` | background from the theme color of another scope |

See `assets/syntax/markdown.syn`: `# Heading` hides the `#` and draws the text
at 1.6x bold, `**bold**` hides the stars, `- item` becomes `• item`, `[text](url)`
shows only the underlined text, and fenced code blocks embed the named
language. Put the cursor on a line to see it as written.

Scope names are shared with the themes (`keyword`, `string`, `comment`,
`function`, `type`, `number`, `heading.1`, `strong`, `link`...). A theme line
is `scope #rrggbb [bold] [italic] [underline]`; dotted scopes fall back to
their parent (`heading.3` → `heading`). `assets/themes/dark.theme` lists every
UI and syntax scope.

Built-in languages: Rust, Markdown, C, C++, Python, JavaScript, TypeScript, Go,
shell, TOML, JSON, YAML, HTML (with CSS/JS embedded), CSS, Lua, Zig, Java, C#,
Ruby, SQL, Makefile, diff, git commit, INI, XML, Haskell, OCaml, Elixir,
Kotlin, Swift, PHP, Nix, Dockerfile, CMake, LaTeX, and quin's own files.

### Importing grammars

```sh
quin convert textmate grammar.tmLanguage.json my-lang.syn
quin convert treesitter mylang highlights.scm my-lang.syn
```

TextMate grammars (what VS Code uses) convert rule by rule; Oniguruma-only
constructs are rewritten when possible and otherwise skipped with a `#
skipped: ...` comment in the output. tree-sitter grammars are real parsers and
cannot be converted faithfully: the converter extracts the keyword lists and
node hints from `highlights.scm` and adds generic rules, giving a starting
point to edit by hand. Drop the result in `~/.config/quin/syntax/`.

## Code map

The code favours plain functions and few abstractions; each file has a
header comment saying what it is for.

| File | What |
|---|---|
| `src/main.rs` | command line, window and input events (winit), presenting frames (softbuffer) |
| `src/editor.rs` | the editor state and key dispatch |
| `src/commands.rs` | every key-bound command, and the `COMMANDS` table |
| `src/keymap.rs` | key type and the default keymaps (the tables are the source of truth for this README) |
| `src/typed.rs` | `:` commands |
| `src/overlays.rs` | prompts and pickers: search, file/buffer/command pickers, global search |
| `src/picker.rs` | fuzzy matching, picker and prompt widgets |
| `src/buffer.rs` | a buffer: rope, file, language, undo history |
| `src/selection.rs`, `src/movement.rs` | ranges and motions (word motions are ported from Helix) |
| `src/history.rs` | transactions and undo/redo |
| `src/frame.rs` | views, split tree, tabs |
| `src/terminal.rs` | pty + terminal emulator (`vte`) |
| `src/tree.rs` | the file tree sidebar and how its edits become file operations |
| `src/syntax/` | `.syn` format parser, highlighter, language registry, converters |
| `src/theme.rs` | themes and style resolution |
| `src/render/` | pixel painter, fonts (discovery, glyph cache, ligature shaping), line layout, drawing the UI |
| `src/files.rs`, `src/search.rs`, `src/clipboard.rs`, `src/config.rs` | directory walking and background grep, regex search, clipboard, configuration |
| `src/tests.rs` | end-to-end tests that replay key sequences |

`cargo test` runs the unit and end-to-end tests; `cargo clippy` is clean.

## Limitations and ideas

- No soft wrap: long lines scroll horizontally.
- One font plus the symbol fallback, with synthetic italic. Characters the
  font lacks show as `�` (CJK needs a font that has them: set `font` in the
  config). Fonts are found by file name, not through fontconfig.
- Terminal: no mouse reporting, no sixel; scrollback is browsed in normal
  mode (`C-esc`).
- No LSP, no tree-sitter, and that is on purpose.
- A WebAssembly build is not wired up yet; the platform-specific parts
  (terminal, clipboard, file system) are confined to a few files for when it is.
