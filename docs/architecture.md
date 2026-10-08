# Architecture

This document describes each Markraft crate, the communication between crates, and the rules that separate them. It is meant to help you find where a change belongs, not to explain APIs.
The crate-level docs (`cargo doc --open`) give more detail. The sections below link to those docs.

Look for the **Invariant** lines. They state what a crate must not know or must
not do, and most of them are easier to keep than to restore.

## Bird's eye view

Markraft is a floating, local-first Markdown notepad for macOS. It reads a folder of Markdown files and edits those files in place.

```text
markraft-app                         Third-party host
    |                                      |
    +----------> markraft-workspace <-------+
                      |          |
                markraft-notes   markraft-gpui <--- markraft-vim
                      |          |       |
                markraft-commonmark      +--> math, syntax, media
                      |          |
                      +----> markraft-core
```

`markraft-notes` owns note identities, source documents, storage, and persistence.
It has no GPUI dependency. A headless consumer uses `NotesLibrary` directly.

`markraft-workspace` supplies the GPUI workspace and cached editor sessions.
The host chooses directories, mounts `WorkspaceView`, and handles `WorkspaceEvent` requests.
The standalone app owns process services, windows, application menus, and update integration.
See [Integration](integration.md) for the public contracts and external consumer example.

When a note opens, `markraft-commonmark` parses the file into an immutable
`markraft-core` tree and retains the original bytes in a `SourceTrack`.
The editor turns input into transactions and renders the resulting state.
The workspace copies committed documents into the notes library.
One persistence worker writes each change through its source track.

`markraft-gpui` uses `markraft-commonmark` only in tests.
The workspace supplies the concrete document kind through core's `kind` contracts.

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
  - `lock` protects the state directory. An additional advisory lock on the notes
    directory prevents two hosts from opening it with separate state directories.
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

The rich-text model provides immutable `Node` trees with value semantics and a `Schema` compiled from a `SchemaSpec`.
Its `ChangeSet`s use token offsets in the starting document. On top of
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
  blocks.
- the comrak-based parser and rule table.
- the canonical serializer, plain text and HTML codecs (`CommonMarkCodecs`).
- the editing extensions: input rules, corrections, auto pairs and the table
  invariant.
- `SourceDocument` / `SourceTrack`, the source-preserving save codec.

This crate is where the **inline source model** lives:

- The text of a paragraph, heading or table cell *is* its Markdown inline
  source, delimiters and escapes included.
- `derive` re-reads that text with comrak and produces every style mark,
  concealed delimiter (`SYNTAX` mark), hard break and atom.
- Formatting commands rewrite source text. They do not toggle marks.
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

### `crates/markraft-syntax`

`highlight` splits code into spans that each carry a `Tone` (text, keyword,
name, string, comment) using syntect grammars, with a bounded per-thread cache.
`Tone::rgb` gives the colour for a light or a dark background.

**Invariant:** spans name roles, never colours, so one pass serves every
appearance and every renderer.

### `crates/markraft-media`

`locate` and `resolve` read image sources against the note's directory and `typora-root-url`.
`ImageType` identifies supported formats by extension or file bytes.
`MAX_IMAGE_BYTES` limits image size. `relative_url` writes a path back as a link.

**Invariant:** the view, exports and pasting all read pictures through this
crate, so a picture the editor shows is one an export carries.

### `crates/markraft-gpui`

The native editor view (`EditorView`). It owns:

- input: key actions, `EntityInputHandler` for IME, clipboard and typeahead.
- the transaction funnel and host guards.
- layout: projection lines → `LayoutLine` → visual rows, shaped lazily around
  the viewport and reused while their `LineKey` matches.
- painting, find, AccessKit text and caret blink.
- screen resources: image loading, code highlighting colours, and a bounded math
  work queue with raster caches.

The view sends `EditorEvent` values to its host, including `Changed`, `LinkClicked`, `WikiLinkClicked`, `FilesPasted`, and `Extension`.
The host supplies document and transaction guards, wiki targets, a remote image fetcher, and image directories. Extensions such as
Vim implement `markraft_gpui::Extension`.

**Invariant:** the view never touches the tree directly. Every edit is a
`TransactionSpec` or a catalogue command, and everything drawn comes from the
state's projection.

**Invariant:** the view does not know about notes, saving or paths, except the
image directories it hands `markraft-media` to resolve pictures against. What a wiki link names and
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

### `crates/markraft-notes`

The notes engine exposes `NotesLibrary`, revision-checked edits, immutable snapshots,
save receipts, and explicit shutdown. It has no GPUI dependency.

- `storage.rs` holds note identities, the library, preferences, and search.
- `vault.rs` holds the store: the rules for saving, conflicts, and external changes.
- `directory.rs` implements `NotesBackend` over a folder of Markdown files.
- `persistence.rs` owns the storage worker and file watcher.
- `doc.rs` supplies the concrete Markdown kind and export snapshots.
- `fs.rs` supplies atomic writes, trash operations, and storage errors.
- `locale.rs` supplies explicit `Message` and `I18n` values.
- `backend.rs` defines `NotesBackend`, the contract for storage that the host owns.
- `engine.rs` defines the session and the save outcome that a host exchanges with the store.
- `conformance.rs` holds the contract checks that a host runs against its backend.

The public facade uses host-selected notes and state directories, or a host backend.
It does not read or write the standalone application's settings.

The store has one implementation of save, refresh, and conflict handling.
It reads and writes notes through `NotesBackend` only.
A Markdown folder and a host backend are the two implementations of that trait.
`NotesBackend` has no file paths, watchers, or manifest, because a database has none of them.
For those, the store calls the Markdown folder directly: a path, a rename that moves the file, the Trash, and the manifest.

**Invariant:** storage wins a conflict, and the local edits are stored before the stored version is shown.
A Markdown folder keeps them as a file beside the note. A host backend keeps them as a note of its own.

**Invariant:** a deletion never removes a version that no editor has seen.
The store drops that deletion, and the note returns as storage holds it.

**Invariant:** every backend write carries an expected revision.
A write that reports a conflict is accepted only when storage already holds exactly that write.

**Invariant:** the ID of a note with a logical key is derived from the key.
The storage identity keeps the key unique, on one device and across devices.

**Invariant:** an asset ID is the SHA-256 of the asset's bytes.
The component never deletes an asset.
The store, the worker, and the settings types are public only with the `unstable-internals` feature.
The workspace and the application enable it. A host does not.

**Invariant:** a save receipt reports completed persistence work.
An empty draft is not reported as a Markdown file on disk.

**Invariant:** successful close stops the storage worker and releases the directory lock.
A failed save keeps the library available for retry.
Close cancels watcher callbacks before it stops storage.
Native watcher cleanup runs separately because FSEvents registration and unregistration can block on a system service.

### `crates/markraft-workspace`

The embeddable workspace owns editor sessions, note navigation, save coordination,
rename flows, daily notes, and note UI.
`WorkspaceView::open()` mounts Markdown folders.
`with_backend()` and `with_session()` mount host-owned storage.
The host handles settings, hide, quit, diagnostic, and locale requests through `WorkspaceEvent`.

`flush` provides a save barrier. `prepare_close` saves committed edits and stops persistence.
The host retains the view until close succeeds.

The optional `unstable-standalone` feature names what only the standalone application uses.
That is its window services, its updater, its instance requests, and its settings window.
The application reaches the view through the `Standalone` trait. A host does not enable this feature.
The optional `sandbox` feature adds the folder grants that the macOS App Sandbox requires.
The optional `native-translation` feature builds the Swift translation bridge.

The workspace has no distribution-channel feature.
It asks the host's update service whether this copy updates itself, and shows update controls only then.

**Invariant:** a type that the workspace builds and the host reads is `#[non_exhaustive]`.
A new preference, option, event, or receipt field does not break a host.

**Invariant:** mounting a workspace does not create process services or replace application menus.
The host owns tray icons, global shortcuts, IPC, updater integration, and process termination.

**Invariant:** only the persistence worker writes notes after the store opens.
The UI sends requests and applies receipts.

**Invariant:** each workspace owns its language and Markdown formatting preferences.
One workspace cannot change another workspace's formatting through global state.

### `crates/markraft-app`

The standalone host owns CLI parsing, single-instance handoff, the floating window,
tray controls, global shortcuts, application menus, settings paths, updates, and crash reports.
`host.rs` adapts process services to the workspace contracts.
`main.rs` mounts the workspace and handles its requests.

The launch channel publishes its private request directory before registering its file watcher.
Watcher registration runs on a separate thread because FSEvents can block on a system service.
Until registration succeeds, the host checks the request directory at most every 250 milliseconds.

The app retains its existing settings and data locations.
The refactor does not require a file-format migration.

### `xtask`, tests, and external consumers

`xtask` builds, signs, and releases the macOS bundle. It also runs local mutation testing.
`crates/markraft-workspace/src/e2e` tests workspace behavior with temporary folders and a controlled clock.
The suites that do not depend on files run a second time over a host backend.
`crates/markraft-notes` tests the headless API and persistence contracts.

`examples/integration` has its own Cargo workspace and lockfile.
It contains a native workspace host and a headless notes consumer.
Neither consumer depends on `markraft-app`.

## Data model

A document is a `Node` tree. Positions are integer token offsets:

- a container contributes an open and a close token.
- text contributes one token per `char`.
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
   order: change filter, transaction filter, transaction extenders, then transaction appenders.
   Corrections run to a fixed point inside transaction extenders.
   Appenders may add follow-up transactions to the chain.
3. Every state field resolves against the final state.
4. The view runs the host's document guard, then its transaction guard, on the
   whole chain. The workspace's **source guard** applies the chain to the note's `SourceTrack`.
   An accepted edit is always one the save path can write.
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
  ranges. A search hit is not automatically a safe replacement.
  `ReadingMatch` identifies exact matches, whole source spans, and composite matches.
  Partial atoms and hidden delimiters need an explicit policy.
- **Layout** invalidates lazily. `Lines::sync` compares the projection `Arc`,
  the width and the shaping revision, and late math results invalidate only
  their rows and containing table grids.

New indexes, such as headings, should derive from the projection or the
analysis in the same way. None of them may introduce a second document model.

## Life of a keystroke

Typing `a` in an open note:

1. GPUI delivers the key through `EntityInputHandler::replace_text_in_range`
   or a bound action for a command key. The view maps it to a catalogue
   command and calls `run_command`.
2. The command returns a `TransactionSpec` that inserts the character. Inside
   `update_with_appended`, commonmark's input rules may rewrite the source.
   `derive` recomputes the line's marks. Corrections repair the shape but leave the caret's line unsettled.
3. The source guard applies the chain to the `SourceTrack`. On success the view
   publishes the new state and emits `EditorEvent::Changed`.
4. The app's session subscription copies `committed_document()` into the
   `Library`, which marks the note changed. It then calls `schedule_save`,
   which bumps the save revision and sets a short deadline.
5. The app's poll loop sees the deadline pass and sends `Save(revision,
   library)` to the persistence worker. The worker coalesces adjacent saves.
6. `Store::save` renders through `SourceTrack` and skips unchanged bytes.
   It backs up the old bytes and writes a temporary file in the same directory.
   After fsync, it checks that disk still contains the expected bytes.
   It renames the temporary file and fsyncs the directory.
7. A `Saved` receipt returns. `SaveState` ignores stale receipts, and
   `Library::acknowledge_saved` clears a note's change only if its generation
   still matches.

An IME session follows the same path, except that marked text lives in the
document as a composition range. `committed_document()` excludes it, so
candidates never reach the `Library`, the disk or an export snapshot.

## Cross-cutting concerns

### Save ordering and barriers

Saves are asynchronous and debounced. Operations that need the latest text on disk use `flush_then`.
It copies committed documents from cached editors into the library and waits for a flush barrier.
If new edits arrive, it repeats the barrier. These operations include ⌘S, rename, and relaunch after an update.

Interactive close uses `prepare_close` to freeze editing, save, and release storage.
Export does not need a barrier: it snapshots the committed note directly. A failed flush cancels a quit. Hiding the window flushes without
waiting.

### External changes

A separate `notify` watcher, plus a refresh on every window activation,
reports changes made by other programs. A host backend reports its changes through a change notifier.

Storage always wins. If local text differs from both the old and new stored contents, the store keeps the local text as a conflicted copy. A change that lands in the middle
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
Its library generation records where it came from. It does not authorise an edit to a later note. Only Markdown export exists today. HTML or PDF export
should derive from a snapshot, never from live screen caches.

### Editor context menus

The editor resolves a secondary click independently of primary-click actions.
It settles the pointer selection and composition, then emits a
`ContextMenuRequested` snapshot containing the document, selection and clicked
object. The app combines the editor's capabilities with note permissions to build
the menu. Editing controls share the same applicability queries.

Clicking within an existing selection preserves it. Otherwise a link selects its
entire label, ordinary text selects the clicked word, and blank space places the
caret. Object-specific commands precede generic editing and formatting commands.

The platform presents an AppKit menu after the selection has painted, outside
GPUI entity and window updates. Each popup owns its native action target.
Popup actions do not consume the process-wide tray event queue. A result is local to one popup: the app validates its
note, editor and snapshot again before executing it. Changing the document,
selection or active note invalidates that result.

**Invariant:** a context menu does not create another editing pipeline. Commands
use the existing clipboard, transactions and source guard, with an isolated undo
boundary for menu edits. Asynchronous attachment imports also have their own
undo boundary.

Native text services use reading-text snapshots: hidden Markdown
syntax and link destinations are not exposed as selected prose. Case transformations
map literal characters back to source spans while retaining formatting. Arbitrary
rewrites require contiguous literal spans or unformatted top-level paragraphs.
Composite selections advertise read-only services rather than discard hidden syntax.

Returned text passes the same source guard, permission check and isolated history.

A word picked across concealed spelling is a `ReadingSelection`, defined in core's `kind` layer.
The selection layer uses only the `SelectionKind` contract. It does not depend on commands or projection.

A command that replaces a custom selection passes a `ReplacementStyle`.
This value specifies whether the content supplies styles or retains the styles of the text it replaces.
`Selection::from_json` reads built-in kinds. `ReadingSelection::from_json` reads this custom kind.

An owned `NSServicesMenuRequestor` temporarily joins the native responder chain.
`NSMenu.popUpContextMenu` inserts system Services and available Writing Tools.
Writeback stays alive after menu tracking and expires when the editor context changes.
Writing Tools can request coordinator context and then return text through the Services
pasteboard bridge. The session records that request so both return paths preserve
unchanged source styles. Ordinary Services still inherit the selection's starting style.

Writing Tools anchors its pasteboard popover through `NSViewContentSelectionInfo`.
The app supplies the selection's first visual line as `selectionAnchorRect` in
native view coordinates. Its session stores that rectangle on the GPUI view,
so AppKit can query it without reentering a GPUI update. An existing view
implementation takes precedence. Closing the session releases its stored anchor.

Translation and sharing use the same selection rectangle. Lookup and detected-data
presentations use its center because their AppKit APIs accept a point. Native text
presentations share one coordinate conversion for flipped and unflipped views.
The transparent translation anchor passes pointer events through to the editor.
The context menu stays at the pointer. Control menus keep their own anchors.

Checking panels retain their action responder across context menus. A text-service
session can sit above that responder and retain it as its restoration target.
Closing the checking session first retires the pending menu and text-service session,
then releases the panel responder. This order preserves the original responder
chain and rejects any queued result from the retired sessions.

Translation uses the public SwiftUI presentation API, weak-linked behind macOS 14.4
availability. The small Swift bridge is compiled by `xcrun swiftc`. It requires an
Xcode SDK providing the Translation framework. Lookup, sharing, speech, and Open With
use public AppKit or AVFoundation APIs. Detected-data menus retain the original
system result and its checked string, including native detector metadata.

Native popover accessibility varies by macOS version. Translation generation,
replacement-button interaction and undo have been verified on macOS 27.
The Rust callback and guarded replacement paths have separate headless test coverage.

Each note owns its spelling document tag, so ignored words remain document-local.
Prose checks run in bounded Unicode chunks after a debounce and yield between chunks.
Checks exclude code, formulas, and URL contents. Cocoa UTF-16 ranges are checked and
converted to document scalar positions. A check returns spelling ranges only. Guesses are fetched for the word a menu asks about.

Diagnostics survive selection changes. An edit moves the diagnostics beside it and removes those it touches, and
the next check restores the ones that still apply.

Smart substitutions run only at a fresh typing or IME commit boundary.
They do not run after paste, undo, or service writeback. At that boundary, a detected link is applied only when its text includes a scheme.
The explicit Add Links command applies every detected link.

Smart deletion and smart copy apply to selections made by double-click or contextual click.
They do not apply to manually extended selections that end on word boundaries. Enabling smart quotes disables
new straight-quote auto-pairs while preserving other delimiters. Settings persist
through the existing preferences pipeline.

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
- The workspace has end-to-end tests over a temporary folder: saves, receipts,
  external changes, rename, daily notes and faults.
- The end-to-end suites that do not depend on files also run over an in-memory
  host backend. A difference between the two kinds of storage fails a test.

Real font rasterization, narrow windows and system input methods need a native
run. Unicode text injected in a test does not exercise a real IME candidate
session.
