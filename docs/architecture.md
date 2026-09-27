# Document and rendering boundaries

Markraft keeps its immutable document tree and transaction history in
`markraft-core`. CommonMark owns parsing, spelling and source-preserving writes.
GPUI owns interaction and screen geometry. The application owns files, note
identity, workspace links and persistence ordering.

## Accepting document state

An editor builds the entire transaction chain, including appended corrections,
and resolves its state fields before running its guards. The source guard must
accept the final document before the editor publishes it. A rejected edit must
leave the document, source baseline and undo history unchanged.

User edits to an open note, including link retargeting after a rename, use this
transaction boundary. Replacing the editor state is reserved for opening or
reloading a document: it deliberately resets history and view state. Batch
operations are atomic within a note, not across the notes folder. Renaming a
file and undoing text edits are separate operations.

`dispatch_isolated` prepares temporary history boundaries around a host edit,
including suspending and resuming an explicit Vim-style undo group. The whole
chain is guarded before publication. Refusal preserves the original group;
success starts a fresh input segment after the independent host edit.

The library holds committed document snapshots. IME candidates remain editor
state until committed and must not enter persistence or exported snapshots.
Existing save generations and flush barriers continue to order disk writes.

## Readable text and positions

`kind::reading` provides layout-independent readable text, atom labels and
character-to-document mappings. GPUI consumes it for find and accessibility.
Screen wrapping, object widths and caret reveal geometry do not define search
positions. CommonMark plain-text serialization keeps its clipboard policy,
including tabs between table cells; workspace search keeps its own ranking and
path matching.

A search hit is not automatically a safe text replacement. `ReadingMatch`
distinguishes exact ranges, complete source spans and composite mappings.
Partial atom labels, hidden delimiters, structural boundaries and partial
Unicode graphemes require an explicit replacement policy. Typed document and
shown-text ranges identify the coordinate space at the new boundary; they are
not UTF-8 source-byte offsets. Search consumers must use hits with the document
snapshot that produced them or map them through subsequent changes.

## Document analysis

`kind::analysis::DocumentAnalysis` pairs one immutable `Projection` with equation
semantics and its numbering options. It can be built without a window. The
editor refreshes it after accepting state, when all state fields have finished
resolving. This avoids recursively reading the state under construction from a
state-field reducer.

Selection-only changes reuse the projection and equation index. Semantic option
changes rebuild equation data; font, scale and width changes belong to layout.
Equation analysis currently scans the document on content changes. The existing
projection remains incremental. Further indexing should follow measurements,
not introduce a second document model.

Heading navigation can add a separate heading index. Footnote, heading and
TeX-label namespaces retain their own rules. Workspace path resolution stays in
the application. Analysis ranges are snapshot-local, not permanent object IDs.

## Math artifacts and screen resources

`markraft-math` turns TeX and logical typesetting options into a self-contained
SVG and baseline metrics. It owns RaTeX and source/complexity/logical-geometry
limits. It has no GPUI dependency.

GPUI rasterizes that artifact at the device scale and owns pixel allocation
limits, textures, the bounded work queue and screen caches. Existing inline
object geometry remains the single source of visual-row baselines, hit testing
and selection geometry. Late results remain keyed by their complete request;
layout invalidates affected formula rows and their containing table grids.

A future export or formula-copy consumer can consume the same SVG directly.
Adding diagrams does not require inventing a renderer registry now.

## Frozen export inputs

`SourceTrack::snapshot` captures immutable source baselines without advancing
the editing track. The application captures the committed note, source baseline,
formatting options and resource base before queueing work. The persistence
worker materializes a `DocumentSnapshot`; Markdown export uses that frozen text.

The snapshot's library generation is scoped to the current library lifetime. It
is provenance, not a globally unique version or permission to apply an edit to
a later note. The snapshot owns its data; later edits, saves, settings changes
and note switches cannot change it. Rendering a snapshot neither marks the note
dirty nor acknowledges a save. A future HTML/PDF export should derive semantic
analysis from this snapshot and its options, never from live screen caches.

## Source editing contract for a later change

Full source mode is not implemented by these boundaries. A source editor needs:

- A `SourceChange` expressed in UTF-8 byte ranges, with boundary validation and
  explicit conversion to document token positions.
- Source revisions that change for whitespace, front matter and other edits
  even when the parsed tree compares equal.
- Exact source-byte history paired with semantic transactions. Undoing a source
  edit must restore its bytes; equal trees alone cannot choose an older spelling.
- Mode switching that preserves history, selection and scroll anchors. It must
  not use `replace_doc` as a mode switch.
- A defined representation for incomplete or unsupported source, and a policy
  for re-entering rich editing without discarding text.

The current `SourceTrack` rule that restores original bytes when the rich
editor undoes to the original tree remains intentional. It must be revised
alongside source-only history, rather than silently repurposed.

## Verification

Focused checks cover mapping precision, formula semantics independent of
layout, baseline-preserving rasterization, source snapshot isolation and
transactional edits. Existing coverage for IME, guarded edits, save receipts,
external changes and table invalidation remains relevant. Native verification
adds real font rasterization, narrow-window layout, search, rename/undo and
export/reload checks. Do not equate injected Unicode text with exercising a
real system input-method candidate session.
