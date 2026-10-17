<p align="center"><img src="assets/icon/icon.png" alt="quin" width="160"></p>

# quin

> [!WARNING]
> Although this text editor targets fully manual (artisanal) coding, it is COMPLETELY vibe-coded with only some sprinkles of human-made tweaks. 
> I do intend to rewrite parts of it by hand, but this will forever be an AI-centric codebase due to 2 reasons:
> 
> 1. I really wanted a code editor that's plug-and-play for my necessities without actually requiring to tweak keybindings, plugins, etc.; and
> 2. I have many more projects that are more interesting to me than a text editor.
> 
> Feel free to open PRs or code suggestions though!

A small, fast, keyboard-driven code editor with a native GUI.

## What is this project

quin is a plain text editor for code: Helix-style modal editing in its own
window, with buffers like neovim. It opens a file or a directory and works
without setup. There is no LSP, no AI and no plugin system, and that is on
purpose.

## How to use

Build it with `cargo build --release`, then run `./target/release/quin`.

```sh
quin                       # welcome page with recent files and directories
quin src/main.rs           # open one or more files
quin ~/project             # a directory becomes the working directory
quin install               # copy the binary to ~/.local/bin and add a desktop entry
quin convert textmate <grammar.json> [out.syn]          # import a syntax grammar
quin convert treesitter <name> <highlights.scm> [out.syn]
quin --screenshot <out.ppm> [--size WxH] [--keys "<keys>"] [FILE ...]   # render one frame, no window
quin --help | --version
```

In the editor:

- `i` types text, `esc` goes back to normal mode.
- `space e` opens a file, `space b` switches buffers, `space f` searches all files.
- `:` opens the command palette (`:write`, `:quit`, `:terminal`, `:theme`...).
- `:config` opens the configuration file.
- `:help` lists every key and command.
