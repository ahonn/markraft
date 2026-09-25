# Architecture improvement validation — 2026-09-26

Validated local changes on top of `183ab3b`, without committing or publishing.
Host: Mac mini, Apple M4 Pro, 48 GB RAM, macOS 27.0 (26A428).
The native checks used a debug build in an ad-hoc signed temporary app bundle,
with a distinct bundle ID, notes directory, and settings file.

## Implemented scope

| Area | Change |
| --- | --- |
| Persistence consistency | Explicit deletion intent, per-note generations, version-matched receipts and external acknowledgments; failed recovery retains local state and its baseline |
| Nonblocking file operations | Worker replies awaited without blocking the UI; shared save barriers; workspace generations reject stale completions |
| Test isolation and diagnostics | Headless tests use explicit refreshes; native watcher tested separately; event wakeups and queue/render/write timing logs |
| Incremental work | Dirty-note saves, cached search text and source tracks, adjacent autosave coalescing without crossing command barriers |
| Workspace boundaries | Pure recovery decisions and operation state; I/O continuations separated from presentation; reload prevents edits and stale event replay |
| Source patches | Transaction-chain validation and a fast path for editing within one existing top-level block; complex changes retain the original conservative fallback |

The source fast path still copies the source string. It does not establish
constant-time editing. The existing 50 ms UI/platform timer remains; storage
events now also wake the UI directly. No claim of improved idle power usage is
made without a before/after measurement.

## Final automated results

| Command | Observed result |
| --- | --- |
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | Passed |
| `cargo test --workspace --locked` | 1479 ordinary tests + 9 doctests passed; 0 failures; 2 ignored |
| `cargo test -p markraft-app --features updater-mock --locked updater::tests` | 3 passed |
| Native watcher test, explicitly run with `--ignored` | 1 passed, 1.07 s |
| Workspace scale test, explicitly run with `--ignored` | 1 passed, 3.08 s |
| `cargo build -p markraft-app --locked` | Passed |
| `git diff --check` | Passed |

All ignored workspace tests were subsequently executed successfully. The updater
feature run overlaps tests in the default suite; 1493 successful executions does
not mean 1493 distinct test cases.

Local logs:

- `/tmp/markraft-all-final.log`
- `/tmp/markraft-clippy-final.log`
- `/tmp/markraft-updater-final.log`
- `/tmp/markraft-watcher-final.log`
- `/tmp/markraft-scale-final.log`

The full app suite includes regressions for consecutive deletes before the first
flush completes, editing during a failed save, recovery failures, stale events,
source-only changes with equal timestamps, and a rename path receipt arriving
before its backlink-update continuation.

## Native user-flow observations

The following checks used the real app, native keyboard/mouse interaction and
file dialogs. File contents were independently inspected after operations.

| Flow | Observed result |
| --- | --- |
| New note, text entry, select/replace, save | Content persisted in a stable Markdown file |
| Chinese, emoji and combining mark paste | `中文编辑 🧪 é` survived save, undo, redo and restart |
| Bold formatting across paragraphs | Rendered formatting and saved Markdown agreed |
| Markdown paste | Heading, quote, ordered/nested lists, inline formatting and fenced code rendered correctly |
| Code editing | A new indented code line remained literal and persisted |
| Task list | Checkbox changes persisted as checked task syntax |
| Table editing | Cell replacement, Tab navigation, row insertion and row deletion persisted; whole-document replacement could be undone back to the table |
| Search and pin | Title/body matches appeared; pinning and opening a result worked |
| Source preservation | Editing a heading kept front matter, CRLF, reference syntax and unusual list spacing; undo followed by save restored the original bytes |
| External file dialog | File opened at its original external path; editing saved there |
| Export | Exported bytes equaled the current document; current editor stayed on the source note |
| Rename conflict | Existing filename was refused without overwriting its file |
| Rename followed immediately by save and switch | Final build updated the resident backlink document; following the rewritten link opened the renamed note |
| External create | Watcher discovered the new file; a save of the current note did not delete it |
| External rewrite of a clean document | The visible document adopted the external content |
| Save failure while continuing to type | Unwritable temporary folder produced a visible error; newer local text remained, and disk retained the old text |
| Recovery failure after an external rewrite | Local editor stayed intact while a conflict copy could not be written |
| Save As during failure | Local text was saved to a writable external rescue file and opened successfully |
| Recovery retry | After restoring permissions, retry wrote the complete local conflict copy and adopted disk content; subsequent save cleared the error |
| Reload from disk | Explicit confirmation discarded the test's unsaved suffix; no old UI snapshot rewrote it afterward |
| External move out of the workspace | Current note disappeared with a clear notification; another note became usable |
| Trash cancel / confirm | Cancel preserved the file; confirm removed only the selected inactive file |
| Delete last document | Empty editor remained usable and could save a new note |
| Folder switch and return | New folder opened, selection was persisted to settings, and returning to the old folder through second-launch IPC succeeded without a stale workspace lock |
| Dirty quit / restart | The final edit reached disk before exit; it and the chosen dark theme survived restart |
| Hide / global hotkey | Closing hid the panel; the isolated shortcut brought it back |
| Vim | Insert/Normal transitions, typing, undo, redo and `gg` navigation worked with actual character input |
| Large note | A 1000-paragraph, approximately 281 KB note opened, accepted an edit at its end, saved and scrolled successfully |

The temporary fixtures and the final native app bundle are retained at the path
recorded in `/tmp/markraft-validation-root`. Fault-injected folder permissions
were restored. No personal note directory was used for edits or deletion.

## Measurements and limits

These are one debug-profile run over small Markdown files, not latency
percentiles or a supported-capacity guarantee:

| Notes | Cold load | Incremental refresh | Save |
| ---: | ---: | ---: | ---: |
| 1000 | 171 ms | 4 ms | 43 ms |
| 10000 | 1539 ms | 34 ms | 91 ms |

Native GUI responsiveness was a functional smoke check, not a performance
benchmark. No before/after input-latency or idle-CPU measurements were collected.

The following are **not claimed as native passes**: Chinese IME candidate
composition/commit/cancel (the automation attempt stayed on the Australian
keyboard layout), native image clipboard or drag-and-drop, VoiceOver speech,
multiple displays/Spaces, network or cloud-sync filesystems, disk-full/power-loss
faults, and an actual Sparkle installation/relaunch. Relevant headless tests do
not replace these platform checks. Finite testing cannot cover every possible
sequence of user actions.

One existing startup behavior was observed: restoring previously opened external
files can activate the external file instead of the last selected workspace note.
`main.rs` already queues those paths as ordinary `OpenPaths` requests; this
behavior was not introduced or changed by this refactor. No content was lost.
