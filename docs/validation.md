# Validation matrix

This matrix describes coverage to execute, not a claim that every row has passed.
Record the tested build, macOS version, command output, and actual observations in
the delivery report. Finite testing cannot exhaust every possible user sequence.

## Automated checks

- Run `cargo fmt --all -- --check`.
- Run `cargo test --workspace` and retain the complete summary, including ignored
  tests. A timeout or interrupted run is not a pass.
- Run `cargo clippy --workspace --all-targets -- -D warnings` when the repository
  baseline allows it; distinguish existing diagnostics from new failures.
- Run the native watcher integration check explicitly if it is ignored in the
  default suite. Headless GPUI tests use explicit folder refreshes, so they do not
  validate FSEvents delivery or require a native run loop for every test.

Headless app tests exercise keyboard bindings, transactions, source round trips,
autosave, manual save, save failures, conflict recovery, switching with independent
undo histories, and opening external files. They do not establish native text
shaping, actual input-method behavior, mouse hit testing, or accessibility quality.

## Real-Mac setup

Use a disposable notes directory and a separate `--settings` path when launching
`markraft-app`. Do not point fault injection, rename, or deletion checks at personal
notes. Keep one additional temporary folder for external files. Record file bytes
before source-preservation checks and compare them after save and undo.

Use the native app for user interactions; inspect the filesystem independently to
verify persistence. A screenshot alone does not prove that a document was saved.

## Editing

| Scenario | Actions | Expected result |
| --- | --- | --- |
| Basic text | Type, select, replace, delete, undo, redo; save and reopen | Text, selection behavior, and persisted content agree |
| Unicode and IME | Enter Chinese through the installed IME, edit its composition, cancel, commit; add emoji and combining marks | No lost or duplicated composition; cursor and deletion respect visible characters |
| Inline styles | Toggle bold, italic, strike, code at caret and across a selection; undo | Formatting stays editable and survives reload |
| Blocks | Create headings, quotes, ordered/unordered/task lists; indent, outdent, split, merge and toggle tasks | Structure and numbering remain valid through save and undo |
| Code blocks | Create fences, type multiline code, indent, leave the block | Code stays literal; navigation can leave the block |
| Tables | Create a table, move with Tab/Shift-Tab, edit cells, add/remove rows and columns, insert a cell line break | Cell selection and Markdown round trip agree |
| Clipboard | Copy/paste plain text, Markdown blocks, lists and table cells; paste an image | No text loss; images have a valid local asset path |
| Links | Insert ordinary/wiki links; follow a link; use a missing or ambiguous target | Correct target or clear unresolved state; no unexpected document mutation |
| Source preservation | Edit CRLF files, front matter, unusual list spacing, reference links and HTML; save, undo, save | Unchanged syntax remains intact; saved bytes parse to the displayed document |
| Large documents | Type and scroll in a long paragraph, many-block note and large table | No visible freeze; record input and layout timings rather than inferring speed |
| Vim | Enable Vim; enter/exit insert mode, navigate, edit and undo | Mode transitions and saved content agree |

## Document management

| Scenario | Actions | Expected result |
| --- | --- | --- |
| New note | Create empty/nonempty notes; autosave; reopen app | Nonempty note persists with stable identity and filename |
| Browse/search | Search by title/body/path; clear query; keyboard-select; pin/unpin | Results and selection update consistently |
| Switch | Edit A, switch to B, edit B, return to A; undo each | Edits persist and undo stays in its own document |
| Open external files | Open one/multiple Markdown files using dialog, Finder or second launch | Files open at their original paths without duplicate imports |
| Rename | Rename saved/unsaved notes; try an existing filename; optionally update backlinks | No overwrite; identity and links follow the successful rename |
| Delete | Cancel and confirm Trash; delete active/inactive/last note | Only the requested document is removed; remaining notes stay usable |
| External create | Create a Markdown file outside the app while editing another; save immediately | New file survives, appears once, and retains its content |
| External rewrite | Rewrite a clean note, then a dirty note | Clean note reloads; dirty text is retained in a conflict copy before replacement |
| External delete/move | Delete/move clean and dirty files externally | Library reconciles; dirty content is recovered before its session is discarded |
| Recovery failure | Make the temporary folder unwritable before a dirty-file conflict; restore access and retry | Local editor remains intact until recovery succeeds; no premature acknowledgment |
| Save failure | Make the temporary folder unwritable, edit and save; restore access and retry | Visible failure, retained edits, successful retry, accurate saved state |
| Overlapping work | Start save/open/rename and immediately switch or keep editing | Late results do not overwrite newer edits or activate an unrelated document |
| Settings/restart | Change supported preferences; close/hide/show; quit with dirty notes; relaunch | Settings and files persist; failed saves do not silently approve quit |
| Native integration | Use menu bar, global hotkey, resize, light/dark theme, keyboard focus and VoiceOver | Native controls and focus remain usable; record unavailable hardware or permissions |

## Measurements

Record document/library size and hardware with each measurement. Separate time
spent waiting in the worker queue from serialization and disk writes. For input,
separate transaction application, source validation, projection and layout. Use
repeated samples for latency percentiles; one responsive interaction is a smoke
check rather than a benchmark. Record hidden-window CPU/wakeups before claiming
an idle-performance improvement.
