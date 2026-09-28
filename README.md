<p align="center">
  <img src="assets/icon/Markraft.png" alt="Markraft app icon" width="96">
</p>

<h1 align="center">Markraft</h1>

<p align="center">
  The floating Markdown notepad for Mac.<br>
  An open source alternative to Raycast Notes.
</p>

<p align="center">
  <img src="https://img.shields.io/badge/macOS-13%2B-lightgrey?style=flat-square" alt="macOS 13+">
  <a href="LICENSE"><img src="https://img.shields.io/github/license/ahonn/markraft?style=flat-square" alt="MIT License"></a>
  <a href="https://github.com/ahonn/markraft/releases/latest"><img src="https://img.shields.io/github/v/release/ahonn/markraft?style=flat-square" alt="Latest release"></a>
</p>

<p align="center">
  <img src="assets/screenshots/hero.gif" alt="A Markraft note formats its Markdown as three points are typed and checked off, then shows the plain .md source underneath" width="640">
</p>

Press <kbd>⌥N</kbd> in any app to write, and again to put it away. Notes are plain Markdown files in a folder you choose, so any other editor can open them too.

<p align="center">
  <a href="https://github.com/ahonn/markraft/releases/latest/download/Markraft.dmg"><b>Download for macOS</b></a>
  · <a href="#installation">Installation</a>
</p>

## Features

- **Floating window.** <kbd>⌥N</kbd> shows or hides it over the current app. It stays on top, grows with the note, and can appear on every Space or on the screen with the pointer. A second hotkey for a new note can be set in Settings.
- **Live Markdown.** Formatting is shown as you type, in the style of Typora; inline marks such as `**` appear around the caret only.
- **Tables, links and more.** Tables edited cell by cell, task lists, `[[wiki links]]` with completion, callouts, footnotes, images and highlighted code blocks.
- **Math.** Native LaTeX formulas with `$…$` inline and `$$…$$` blocks. Type `$$` and press Return to start a block; edit its source with a live preview, then press <kbd>⌘Return</kbd> to continue below it. Move the caret into a formula to edit it again. Invalid formulas keep their source and show an error. Rendering works offline through [RaTeX](https://github.com/erweixin/RaTeX). Enable **Automatically number math blocks** in Settings → Markdown for document-order numbering. Use `\tag{A}` / `\tag*{A}` for manual tags, `\label{eq:energy}` with `$\ref{eq:energy}$` or `$\eqref{eq:energy}$` for references, and ⌘-click a standalone reference to jump to its equation. `\notag`, `\nonumber`, and starred equation environments suppress automatic numbers. Manual tags do not consume the automatic counter. Numbering is derived without rewriting Markdown; this version assigns one number per display block, not AMS per-row numbers.
- **Plain files.** Each note is a `.md` file. A save rewrites only the lines you edited and leaves the rest of the file unchanged.
- **Keyboard.** <kbd>⌘K</kbd> lists every action with its shortcut, <kbd>/</kbd> inserts a block, and vim mode can be turned on in Settings.
- **Native.** Written in Rust and drawn with [GPUI](https://www.gpui.rs), without a web view.

<p align="center">
  <img src="assets/screenshots/actions.gif" alt="A table inserted from the action list opened with ⌘K" width="46%">
  <img src="assets/screenshots/find-a-note.gif" alt="A note found and opened with ⌘P" width="46%">
</p>

## Installation

Download [Markraft.dmg](https://github.com/ahonn/markraft/releases/latest/download/Markraft.dmg) from the [latest release](https://github.com/ahonn/markraft/releases/latest), open it, and drag Markraft to Applications. Markraft is signed and notarized, and updates itself through Sparkle. It needs macOS 13 or later.

Markraft runs from the menu bar and has no Dock icon.

## Shortcuts

| Shortcut | Action |
| --- | --- |
| <kbd>⌥N</kbd> | Show or hide the window |
| <kbd>Esc</kbd> | Hide the window (`:q` in vim mode) |
| <kbd>⌘N</kbd> | New note |
| <kbd>⌘P</kbd> | Find a note |
| <kbd>⌘K</kbd> | All actions |
| <kbd>⌘O</kbd> | Open a Markdown file |
| <kbd>⌘,</kbd> | Settings |
| <kbd>/</kbd> | Insert a heading, list, table, code block and more |
| <kbd>[[</kbd> | Link to another note |

## Vim mode

Turn on **Vim mode** in Settings → Editor. The editor starts in Normal mode and shows the current mode.

<p align="center">
  <img src="assets/screenshots/vim.gif" alt="A task moved with dd and p, text appended with A, and the window hidden with :wq" width="480">
</p>

- **Modes:** Normal, Insert, Visual and Visual Line. <kbd>Esc</kbd> returns to Normal mode.
- **Motions:** `h` `j` `k` `l`, `w` `b` `e`, `0` `^` `$`, `gg` `G`. `j` and `k` move by visual row in wrapped lines.
- **Editing:** `d`, `c` and `y` with a motion or doubled (`dd`, `cc`, `yy`), `D`, `C`, `x`, `p`, `P`, `u` and <kbd>Ctrl</kbd>+<kbd>R</kbd>. Counts work, as in `3j` or `2d3w`.
- **Entering Insert mode:** `i`, `a`, `I`, `A`, `o`, `O`. Everything typed in one Insert session is undone in one step.
- **Text objects:** `iw` `aw`, `i"` `a"` (and `'`, `` ` ``), `i(` `a(` (or `b`), `i[` `a[`, `i{` `a{` (or `B`), `i<` `a<`, `ip` `ap`, after an operator or in Visual mode.
- **Markdown blocks:** `dd` and `yy` take a whole block, so a list item keeps its nesting when pasted and a code block is taken whole. In a table, a line is a table row.
- **Commands:** `:` opens the action list as a command line. `:w` saves, `:q` hides the window, `:wq` and `:x` save and hide, `:qa` quits Markraft, `:wqa` saves and quits, `:e` reloads the note from disk, `:enew` starts a new note, `:ls` lists notes, and `:u` and `:red` undo and redo. In Normal mode <kbd>Esc</kbd> does not hide the window; use `:q`.
- **Search:** in Normal mode, `/` opens find with a live preview. Return confirms and returns to the note; Escape restores the original position and query. `n` finds the next match from the cursor and `N` the previous one, wrapping at either end. Search matches visible text literally, ignoring case, and shares its query with <kbd>⌘F</kbd>.

Search counts, operator or Visual-mode search, `?`, regular expressions, `f` and `t`, `.` repeat, registers and other `:` commands are not supported.

## Your notes

Notes are Markdown files in `~/Documents/Markraft` by default. You can choose another folder, including one you already keep notes in, and open any other `.md` file on your Mac.

- Changes made by other apps show up in the open note. If another app changes a file at the same moment you edit it, your version is kept as a separate conflicted copy.
- Deleting a note moves its file to the Trash.
- To sync notes between Macs, put the folder in iCloud Drive or another sync tool. Settings and window state stay on each Mac in `~/Library/Application Support/Markraft`.

## Settings

- **General:** launch at login, the hotkeys, light or dark appearance, and how the window floats, hides and which note it opens with.
- **Editor:** font, text size, line height and width, vim mode, and whether images linked from the web load.
- **Markdown:** the markers Markraft writes for lists, emphasis, code blocks and line breaks.
- **Files:** the notes folder, and where new notes and pasted images are saved and how they are named.

## Privacy

Markraft has no account and sends no telemetry. It connects to the network only to check for updates and to load images that notes link to on the web; both can be turned off in Settings.

## Crash reports

Logs and crash reports are written to `~/Library/Logs/Markraft` and stay on your Mac. After a crash, Markraft mentions the report on its next launch. **Report an Issue…** in <kbd>⌘K</kbd> opens a GitHub issue with your Mac and Markraft versions filled in; attach the report if there is one.

## Contributing

Bug reports and feature requests are welcome as issues. Pull requests are open to collaborators only, so to propose a change, please open an issue and describe it there.

To build from source:

```sh
bash scripts/download-sparkle.sh   # fetch the Sparkle framework into target/sparkle
cargo run -p markraft-app          # run the app
cargo test --workspace             # run the tests
cargo xtask bundle                 # build target/debug/bundle/osx/Markraft.app
```

The toolchain is pinned in `rust-toolchain.toml`. The workspace is split into `markraft-core` (document model and editing), `markraft-commonmark` (Markdown), `markraft-gpui` (the editor view), `markraft-math` (window-independent LaTeX typesetting), `markraft-vim` and `markraft-app`. See [the architecture notes](docs/architecture.md) for document, source, analysis and rendering boundaries.

See [the localization guide](docs/localization.md) for message resources, language preferences, and adding a supported language. To contribute a translation, attach the message files to an issue.

## Acknowledgments

Markraft borrows ideas from apps we like:

- [Raycast Notes](https://www.raycast.com/core-features/notes): a note one hotkey away, floating over whatever you are doing.
- [Typora](https://typora.io): Markdown formatted as you type, with the syntax shown only where you edit.
- [Obsidian](https://obsidian.md): notes kept as plain files in your own folder, with `[[wiki links]]` and callouts.

Built on [GPUI](https://www.gpui.rs), with [comrak](https://github.com/kivikakk/comrak) for parsing Markdown, [syntect](https://github.com/trishume/syntect) for highlighting code and [Sparkle](https://sparkle-project.org) for updates.

## License

[MIT](LICENSE)
