# Architecture

This document describes the high-level architecture of Markraft: what each
crate is for, how the pieces talk to each other, and the rules that keep them
apart. It is meant to help you find where a change belongs, not to explain APIs.
The crate-level docs (`cargo doc --open`) go deeper; the sections below link
to them instead of repeating them.

Look for the **Invariant** lines. They state what a crate must not know or must
not do, and most of them are easier to keep than to restore.

## Bird's eye view

Markraft is a floating, local-first Markdown notepad for macOS. Its input is a
folder of Markdown files; its output is the same files, edited in place.

```
            ┌──────────────────── markraft-app ────────────────────┐
  disk ◀──▶ │ Store ◀─ persistence worker ◀─ Library ◀─ sessions   │
            └───────────────────────────────────────────┬──────────┘
                                                        │ EditorEvent / guards
            ┌─ markraft-vim ─┐   ┌──────────── markraft-gpui ─────────────┐
            │  (extension)   │──▶│ EditorView: input → transaction → paint │
            └────────────────┘   └───────┬───────────────────┬────────────┘
                                         │                   │
                    ┌────────────────────▼───┐     ┌─────────▼──────┐
                    │     markraft-core      │     │ markraft-math  │
                    │ tree · changes · state │     │  TeX → SVG     │
                    └────────────▲───────────┘     └────────────────┘
                                 │
                    ┌────────────┴───────────┐
                    │  markraft-commonmark   │
                    │ schema · codecs · source│
                    └────────────────────────┘
```

When a note opens, `markraft-commonmark` parses the file into an immutable
`markraft-core` tree and keeps the original bytes in a `SourceTrack`. The
`markraft-gpui` editor turns every key, IME update and command into a
transaction and draws the resulting state. The app copies each committed
document into its `Library` and a single worker thread writes it back through
the same `SourceTrack`, patching only the bytes the edit touched.

The dependency graph points one way: `app → vim → gpui → core ← commonmark`,
with `gpui → math` and `app → commonmark`. `markraft-gpui` uses
`markraft-commonmark` only in tests. It learns about a concrete document kind
through core's `kind` contracts, which the app fills in.

## Storage model

**Invariant:** the Markdown file on disk is the only source of truth for a
note's text. There is no database, and no application metadata is written
into the notes folder.

- The notes folder defaults to `~/Documents/Markraft` and can be overridden
  with `--dir` or in settings. `.git`, `.obsidian`, `.markraft`, `.trash`,
  `node_modules` and symlinks are skipped.
- Settings live in `~/Library/Application Support/Markraft/settings.json`.
  Per-folder state lives next to them under `workspaces/<hash>/`:
  - `manifest.json` maps path → note identity (a UUID), pinned state and
    workspace settings such as daily notes.
  - `backups/` holds the previous bytes of each overwritten note.
  - `lock` holds the folder exclusively for one process.
- A note is identified by its UUID, not its path. A new note has no path until
  its first save names it. After that the name only changes through an explicit
  rename.
- The only in-memory index is the search cache in `Library`. Search is a
  substring match over plain text and folder-relative paths.

Everything the app knows about a note in memory is derived from the file and
can be rebuilt by reading the folder again. The `Library` holds the committed
document per note, plus a change generation that tells the save path what is
still unwritten.

## Code map

### `crates/markraft-core`

A general-purpose rich-text model: an immutable `Node` tree with value
semantics, a data-driven `Schema` compiled from a `SchemaSpec`, and
`ChangeSet`s expressed in token offsets of the starting document. On top of
that sits the editing state machine: `EditorState`, `Transaction`,
`Extension`, `Facet` and `StateField`, in the style of CodeMirror 6.

Modules built on those layers:

- `history` stores inverted change sets and groups them by time, user-event
  class and adjacency, with explicit undo groups on top.
- `composition` keeps IME marked text as state.
- `corrections` repairs shapes the schema cannot forbid.
- `commands` holds pure `EditorState → Option<TransactionSpec>` functions.
- `projection` is a flat, line-oriented view in char and UTF-16 offsets.
- `kind` is the contract a view needs from a document kind: roles, codecs,
  conceal, key chains, readable text and analysis.

**Invariant:** core knows nothing about Markdown, HTML, GPUI or files. The
model, changes and command catalogue are parameterised by type ids. Only `kind`
reads roles, and `kind::chains` is the one place that builds commands from
them.

**Invariant:** `history`, `composition` and `corrections` never depend on one
another. When one has to affect another, it goes through the annotations and
effects in `state::protocol`.

**Invariant:** changes in one set never compensate for each other. `Fit`
repairs a splice that would produce an invalid tree. A repair that widens into
another change is an error, not a guess.

### `crates/markraft-commonmark`

The CommonMark/GFM document kind. It provides:

- the schema preset (`commonmark_schema_spec`), which consumers may extend with
  blocks;
- the comrak-based parser and rule table;
- the canonical serializer, plain text and HTML codecs (`CommonMarkCodecs`);
- the editing extensions: input rules, corrections, auto pairs and the table
  invariant;
- `SourceDocument` / `SourceTrack`, the source-preserving save codec.

This crate is where the **inline source model** lives:

- The text of a paragraph, heading or table cell *is* its Markdown inline
  source, delimiters and escapes included.
- `derive` re-reads that text with comrak and produces every style mark,
  concealed delimiter (`SYNTAX` mark), hard break and atom.
- Formatting commands rewrite source text; they do not toggle marks.
- A canonicalising correction settles a line only once the caret leaves it.

**Invariant:** nothing but `derive` authors a style mark. A new inline style is
a row in `styles` plus a mark in the schema, never a parse rule.

**Invariant:** nothing is silently lost. Any construct without a node type is
kept as `raw_block` or `raw_inline` and written back verbatim.

**Invariant:** `SourceTrack` owns the bytes around the document. Front matter,
BOM, line endings and untouched blocks come from the original file. An edit it
cannot map to source is refused (`SourceError::UnsupportedEdit`), never
approximated. A document equal to its origin writes the original bytes.

### `crates/markraft-math`

`typeset` turns TeX into a self-contained SVG plus baseline metrics using
RaTeX. It owns source-size, complexity and layout limits.

**Invariant:** no GPUI, no window system, no raster decisions. A future export
or formula-copy consumer can use the same artifact.

### `crates/markraft-gpui`

The native editor view (`EditorView`). It owns:

- input: key actions, `EntityInputHandler` for IME, clipboard and typeahead;
- the transaction funnel and host guards;
- layout: projection lines → `LayoutLine` → visual rows, shaped lazily around
  the viewport and reused while their `LineKey` matches;
- painting, find, AccessKit text and caret blink;
- screen resources: image loading, syntect highlighting, and a bounded math
  work queue with raster caches.

It talks to its host through `EditorEvent` (`Changed`, `LinkClicked`,
`WikiLinkClicked`, `FilesPasted`, `Extension`, …), through guards
(`with_document_guard`, `with_transaction_guard`), and through small resolvers
(wiki targets, remote image fetcher, image base directory). Extensions such as
Vim implement `markraft_gpui::Extension`.

**Invariant:** the view never touches the tree directly. Every edit is a
`TransactionSpec` or a catalogue command, and everything drawn comes from the
state's projection.

**Invariant:** the view does not know about notes, saving or paths, except the
image directories it resolves pictures against. What a wiki link names and
what a pasted file becomes are the host's questions.

**Invariant:** screen geometry does not define document positions. Wrapping,
object widths and caret reveal belong to layout. Search and accessibility
positions come from `kind::reading`.

### `crates/markraft-vim`

Modal editing as an opt-in `Extension`. It is registered on an editor, not
wrapped around it. Its key bindings apply only while an instance puts
`vim_mode` into the key context, and a Vim "line" is a projection line. An
Insert session is one explicit undo group. Every other command is one
transaction.

### `crates/markraft-app`

The application: window and panel, tray, hotkeys, settings, localization,
updates (Sparkle), crash reports and everything about files.

- `main.rs`: CLI, the single-instance handoff, and the one floating window
  whose root is `MarkraftApp`.
- `app/`: the root entity and its parts: `sessions` (cached `EditorView`s per
  note), `workspace` (`SaveState`, operation epochs), `io` (`flush_then`),
  `rename`, `daily_notes`, `find`, `preferences`, and the UI under `app/ui/`.
- `storage.rs`: `Note`, `Library`, `Settings`, `Preferences` and search.
- `vault.rs`: `Store`, the on-disk note store: scanning, manifest, saving,
  renaming, trashing and external-change detection.
- `persistence.rs`: the worker thread that owns the `Store`, plus the watcher.
- `doc.rs`: the concrete document kind the editor is built with.
  `doc/snapshot.rs` holds frozen snapshots for export.
- `fs.rs`: `atomic_write`, trash and `StoreError`.
- `locale.rs`: `Message` and `I18n`. See [localization.md](localization.md).

**Invariant:** only the persistence worker touches the notes folder. The UI
thread sends requests and applies receipts; it never writes a note itself.

**Invariant:** localization is passed explicitly. Shared crates do not own
language preferences, and `I18n` is not process-global state.

### `xtask`, `crates/markraft-app/src/e2e`

`xtask` builds, signs and releases the macOS bundle, and runs local mutation
testing. The end-to-end tests drive a real `MarkraftApp` against a temporary
folder under a controlled clock.

## Data model

A document is a `Node` tree. Positions are integer token offsets:

- a container contributes an open and a close token;
- text contributes one token per `char`;
- any other leaf contributes one token.

Several coordinate spaces meet at the crate boundaries, and mixing them up is
the most common kind of bug:

| Space | Unit | Used by |
|---|---|---|
| document | token offset | changes, selection, commands |
| projection | char / UTF-16 per line | layout, IME, Vim motions |
| shown text | readable char (`ShownRange`) | find, accessibility |
| source | UTF-8 byte | `SourceTrack` only |
| selectable units | AccessKit text position | accessibility |

Conversions are explicit. Ranges are snapshot-local: a position from one
document means nothing in another until it is mapped through the changes
between them.

### Accepting a transaction

1. A command or input handler produces a `TransactionSpec`.
2. `EditorState::update_with_appended` builds the transaction. Hooks run in
   order: change filter, transaction filter, transaction extenders (corrections
   run here, to a fixed point, inside the same transaction), then transaction
   appenders, which may add follow-up transactions to the chain.
3. Every state field resolves against the final state.
4. The view runs the host's document guard, then its transaction guard, on the
   whole chain. The app's transaction guard is the **source guard**: it applies
   the chain to the note's `SourceTrack`, so an accepted edit is always one the
   save path can write.
5. Only then does the view publish: it swaps the state, syncs
   `DocumentAnalysis`, emits `Changed`, and runs extension updates.

**Invariant:** a rejected edit leaves the document, the source baseline and the
undo history exactly as they were. The rejection is reported through
`take_edit_error`.

**Invariant:** undoing a transaction undoes its corrections with it, because
they are part of the same transaction.

Host edits to an open note, such as link retargeting after a rename, go
through `dispatch_isolated`. It closes any open undo groups, marks the edit
isolated in history, reopens the groups afterwards, and passes through the same
guards. `replace_doc` is reserved for opening or reloading a note. It
deliberately resets history, find and scroll.

### Derived state

- **Projection** is cached in a state field and rebuilt incrementally.
- **`DocumentAnalysis`** pairs a projection with the equation index and its
  numbering options. It is built outside any reducer, after the state has
  resolved, and needs no window. Selection-only changes reuse it.
- **Readable text** (`kind::reading`) maps shown characters to document
  ranges. A search hit is not automatically a safe replacement: a
  `ReadingMatch` says whether it is exact, a whole source span or a composite,
  and partial atoms or hidden delimiters need an explicit policy.
- **Layout** invalidates lazily. `Lines::sync` compares the projection `Arc`,
  the width and the shaping revision, and late math results invalidate only
  their rows and containing table grids.

New indexes, such as headings, should derive from the projection or the
analysis in the same way. None of them may introduce a second document model.

## Life of a keystroke

Typing `a` in an open note:

1. GPUI delivers the key through `EntityInputHandler::replace_text_in_range`
   (or a bound action for a command key). The view maps it to a catalogue
   command and calls `run_command`.
2. The command returns a `TransactionSpec` that inserts the character. Inside
   `update_with_appended`, commonmark's extensions run: input rules may rewrite
   the source, `derive` recomputes the line's marks, and corrections repair the
   shape, but they leave the caret's own line unsettled.
3. The source guard applies the chain to the `SourceTrack`. On success the view
   publishes the new state and emits `EditorEvent::Changed`.
4. The app's session subscription copies `committed_document()` into the
   `Library`, which marks the note changed. It then calls `schedule_save`,
   which bumps the save revision and sets a short deadline.
5. The app's poll loop sees the deadline pass and sends `Save(revision,
   library)` to the persistence worker. The worker coalesces adjacent saves.
6. `Store::save` renders the note through its `SourceTrack`, skips it if the
   bytes are unchanged, backs up the old bytes, and writes atomically: a temp
   file in the same directory, fsync, a re-check that the disk still holds the
   expected bytes, rename, then a directory fsync.
7. A `Saved` receipt returns. `SaveState` ignores stale receipts, and
   `Library::acknowledge_saved` clears a note's change only if its generation
   still matches.

An IME session follows the same path, except that marked text lives in the
document as a composition range. `committed_document()` excludes it, so
candidates never reach the `Library`, the disk or an export snapshot.

## Cross-cutting concerns

### Save ordering and barriers

Saves are asynchronous and debounced. Operations that must see the latest text
on disk go through `flush_then`: it pulls every cached editor's committed
document into the library, waits for a flush barrier, and loops if new edits
arrived meanwhile. These are ⌘S, quit, rename and relaunch after an update.
Export does not need one: it snapshots the committed note directly. A failed flush cancels a quit. Hiding the window flushes without
waiting.

### External changes

A `notify` watcher on the worker, plus a refresh on every window activation,
reports changes made by other programs. Disk always wins. If the local text
differs from both the old and the new disk contents, the local text is first
written beside the file as a conflicted copy. A change that lands in the middle
of a save produces a conflicted copy too. Affected notes hold back saves until
the app acknowledges the change. An explicit reload refuses edits (through the
source guard) until it finishes.

### Rename and links

Renaming moves the file without replacing an existing one and keeps the note's
UUID, after a flush barrier. The app then retargets wiki links in other notes:

- In open notes it edits through `dispatch_isolated`.
- In unopened notes it validates the change against the note's `SourceTrack`
  before updating the library.

Batches are atomic per note, not across the folder. A rename and the text edits
it causes are separate undo histories.

### Export snapshots

Export captures the committed note, its source baseline
(`SourceTrack::snapshot`), formatting options and resource base on the UI
thread. The worker renders a `DocumentSnapshot` from that frozen input.

A snapshot owns its data, so later edits, saves and settings changes cannot
alter it. Rendering it neither marks the note dirty nor acknowledges a save.
Its library generation records where it came from; it does not authorise an
edit to a later note. Only Markdown export exists today. HTML or PDF export
should derive from a snapshot, never from live screen caches.

### Screen resources

Images, math rasters and syntax highlighting are view-owned caches with
explicit bounds. They are keyed by their complete request and invalidated
through the shaping revision. Math typesetting runs on the background executor
through a bounded queue.

**Invariant:** inline object geometry is the single source of visual-row
baselines, hit testing and selection geometry, whatever the object is.

### Testing

- Core and commonmark are tested as pure functions: round trips, change
  algebra, corrections and source patches.
- The editor is tested headlessly with GPUI's test context: IME, guards, layout
  invalidation and find.
- The app has end-to-end tests over a temporary folder: saves, receipts,
  external changes, rename, daily notes and faults.

Real font rasterization, narrow windows and system input methods need a native
run. Unicode text injected in a test does not exercise a real IME candidate
session.
