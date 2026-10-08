# Integrating Markraft

Markraft provides a GPUI workspace and a notes service without GPUI.
The standalone application consumes these components and owns its process services.

| Requirement | Crate |
| --- | --- |
| Complete note workspace in an existing GPUI window | `markraft-workspace` |
| Notes, search, and persistence without a view | `markraft-notes` |
| Custom editor interface and document behavior | `markraft-gpui` with `markraft-core` and `markraft-commonmark` |

The [external consumers](../examples/integration/README.md) form a separate Cargo workspace.
They use public interfaces and never include application source files.
Their data directories are temporary and independent of existing Markraft data.

## Host ownership

The host owns the GPUI application, windows, application menus, global shortcuts, tray, updater, and process exit.
The workspace handles note editing and emits requests for host actions.
The host decides whether to implement each request.

For Markdown storage, provide separate notes, state, and cache locations.
The notes directory contains Markdown files.
The state directory contains component recovery and workspace data.
For a custom backend, pass the backend to the workspace constructor.
Database notes do not need a Markdown directory or a fabricated file path.

`WorkspaceOptions` holds only the cache directory and the editor preferences.
Start from `WorkspaceOptions::default()` and set the fields that the host needs.
A later release can add fields to the options and to the preferences.

The cache directory contains replaceable component data.
Keep host preferences separate from component state.

The integration examples pass explicit directories below a temporary root.
Production hosts must select persistent directories and retain access for the component's entire lifetime.
Sandboxed hosts must retain their own file-access grants while workers and readers use those directories.

## Use the notes service

`NotesLibrary::open(NotesConfig::new(notes_directory, state_directory))` opens a service without a GPUI application.
`create()` accepts Markdown and returns a `NoteId`.
The service preserves exact Markdown, including front matter and original whitespace.
For a file backend, `create_file()` also assigns an explicit relative file path.

`search()` returns lightweight `NoteSummary` values without document content.
`note()` returns a `NoteSnapshot` with Markdown and a structured document.
`edit()` requires the snapshot revision and rejects a stale revision.
The editor revision belongs to one open library lifetime.
A backend's `StorageRevision` is a separate durable compare-and-swap token.
Never use the editor revision as a synchronization token.

Await `flush()` to receive a `SaveReceipt`.
The receipt distinguishes saved Markdown files from drafts kept in recovery storage.
An empty scratch note does not require an empty Markdown file.
Await `close()` before opening the same storage in another service.
If either operation fails, inspect the error and retain the service for recovery or retry.
The [notes consumer](../examples/integration/notes-cli/src/main.rs) shows the full sequence.

Startup and search are synchronous operations.
Use the host's background executor when a large directory could delay the interface.
The save and close futures do not require a particular asynchronous runtime.

## Supply a storage backend

Implement `NotesBackend` in the host, then pass it to `NotesLibrary::from_backend()` or `WorkspaceView::with_backend()`.
The backend runs on the component's storage worker.
It requires `Send`, but does not require `Sync` or a specific asynchronous runtime.

`load()` returns exact Markdown, stable IDs, durable revisions, optional titles, and local workspace state.
A backend note has no file path.
`read()` returns the live notes among the IDs that it receives.
Its default implementation calls `load()`.
Override it when a full load is expensive.

`commit()` receives an expected revision and rejects stale writes.
A missing expected revision means create-only.
It never authorizes an unconditional overwrite.
A deleted note counts as absent.
A create replaces its tombstone, and a write that expects a revision of a deleted note is a conflict.
A conflict reports the revision of the live note, or no revision when the note is absent or deleted.

The component assigns every note ID, and the backend must store a note under the ID that it receives.
A note with a logical key has an ID that `NoteId::for_logical_key()` derives from the key.
Two devices that create the same daily note offline therefore create one record, not two.
The backend needs no separate uniqueness constraint for logical keys.

After a commit fails, the component can send the same write again.
If the first commit succeeded but its reply was lost, the retry reports a conflict.
The component then reads the note, and accepts the stored note when it equals the write.

When any other commit reports a conflict, the stored note wins.
The component first commits the local Markdown as a new note.
The title of that note ends with `(conflicted copy <date>)`.
The component then reads the stored note and shows it.
The save receipt reports the note as a conflict, not as saved.
One conflict creates one copy, also when a retry finds the conflict again.

A deletion that reports a conflict is not sent again.
The component restores the note and reads the stored version.

Each note commit must atomically persist content, its new revision, and any pending synchronization marker.
A save can contain several note commits.
A backend does not need to offer a transaction across every note in a save.
Keep the component alive after a save failure so the user can retry.

The [SQLite reference backend](../examples/integration/sqlite-backend/src/lib.rs) implements this contract in an independent consumer package.
It stores exact Markdown in SQLite without a directory of Markdown copies.
Its transactions update the current note, immutable revision history, a durable change log, and a dirty marker.
Deleted notes remain as tombstones.

The example exposes `pending_sync()` and `acknowledge(id, sent_revision)` to demonstrate durable upload intent.
An acknowledgement for an older revision cannot clear a newer edit's dirty marker.
These methods do not upload data.
The example does not implement CloudKit, remote merges, account isolation, or a production synchronization engine.

The host owns its production schema and migrations.
Use the reference backend to understand the contract or start a host-specific implementation.
Keep a local database outside iCloud Drive.
Synchronize records through the host's synchronization service, instead of copying an active SQLite database between devices.

An `AssetId` is the SHA-256 of the asset's bytes.
Equal content has one ID on every device, and an ID never identifies different content.
The component refuses a write whose ID does not match its content.
It also refuses bytes that the backend returns under the wrong ID.
The reference backend stores attachment bytes in SQLite.
The host can use a separate content store if its backend preserves the same identity and durability rules.

The component never deletes an asset, because undo, history, and another device can still refer to it.
To reclaim space, call `asset_references()` on the Markdown of every note that the host keeps.
Then delete the stored assets that no note refers to.

The workspace stores image references as `markraft-asset:` URIs.
The editor loads them through a loader that is separate from the remote image fetcher.
It loads these local assets even when remote images are disabled.
Markdown export writes attachment files beside the exported document.
HTML, PDF, and rich text exports embed the captured image bytes.
Workspace preferences and selection remain local state.

`flush()` confirms local persistence.
It does not confirm that CloudKit or another remote service received the changes.
Display save status separately from synchronization status.

A host that imports remote changes must tell the active component.
The backend receives a `ChangeNotifier` through `set_change_notifier()`.
Call `changed(ids)` with the IDs of the changed notes, or `changed_all()` when the host cannot name them.
The notifier carries no content.
The storage worker reads the named notes again through the backend, so the backend remains the only source of truth.

An embedding host can also call `WorkspaceView::refresh_notes_from_storage(ids, cx)` or `refresh_from_storage(cx)`.
Retain conflicting remote and local content until the user or a merge policy resolves it.

To test a backend, enable the `conformance` feature of `markraft-notes` in the host's test dependencies.
`markraft_notes::conformance::check()` runs the contract checks against the host's real storage.
The reference backend runs them in its own tests.

## Embed the workspace

Call `bind_workspace_keys(cx)` once from the host's GPUI initialization.
For Markdown storage, create an entity with `WorkspaceView::open(NotesConfig::new(notes, state), options, window, cx)`.
For host storage, use `WorkspaceView::with_backend(backend, options, window, cx)`.
This convenience constructor loads the backend synchronously.
The example uses it for a small temporary database.

For production storage, prepare the session on the host's background executor:

1. Validate `options.preferences` before starting the load.
2. Call `NotesSession::from_backend(backend, Default::default())` on the background executor.
3. Transfer the resulting session to the UI thread.
4. Mount it with `WorkspaceView::with_session(session, options, window, cx)`.

`with_session()` returns the view without loading the backend again.
The session and its editors share the same Markdown style handle.
If the host cancels mounting after a successful load, drop the prepared session.
Dropping the session requests storage-worker shutdown.

Render the workspace entity inside the host's layout.

Retain subscriptions to `WorkspaceEvent` while the host uses the workspace.
Handle the events that the host supports, and ignore the others.
A later release can add events.
Handle `OptionsChanged` by saving the editor preferences in the host's own configuration.
The example writes `host-preferences.json` beside its temporary directories, outside the component state directory.

The [graphical consumer](../examples/integration/workspace-host/src/main.rs) provides host-owned save and close controls.
It also routes the native close button and the host's quit action through the component close operation.
Its settings and support handlers display host status messages.
They do not open Markraft application windows.

`flush(window, cx, callback)` saves the currently committed revision.
Later edits remain dirty.
The callback receives a result and the workspace's window and context.

`SaveReceipt.notes` contains the outcomes of note commits in that save request.
It is not the total number of stored notes.
An empty list can mean that autosave already persisted every change.
`markdown_paths` lists file paths only when the backend uses Markdown files.
Database saves do not need to return file paths.

Retain both the workspace entity and its window until the callback completes.
Removing the workspace from the layout does not cancel the callback.
Destroying its window cancels the callback.
Use `window.defer()` before updating an ancestor entity from the callback.
This prevents a synchronous failure callback from entering an ancestor that is still updating.

If the host switches workspaces before completion, the callback returns `WorkspaceError::Superseded`.
Treat that result as a failed request and retry against the intended workspace if needed.

`prepare_close(window, cx, callback)` freezes editing and cancels the current input composition.
It saves committed content and waits for the persistence worker to release its lock.
Close cancels watcher callbacks before completion.
Native watcher cleanup can continue in the background.
On failure, the workspace becomes editable again.
On success, remove the workspace entity or close the host window.

Each workspace remains bound to the window supplied when it opens.
To move notes to another window, complete close and open a new workspace there.

If another operation is pending, close returns `WorkspaceError::Busy` without freezing the workspace.
Retain the workspace and retry after that operation completes.

Register the host's application quit hook for system-initiated termination.
Capture `flush_on_system_quit(cx)` before returning the future that the hook awaits.
Report a failed result through the host's logging mechanism.
That hook cannot offer the interactive close flow's retry decision.
The pinned GPUI version waits at most 200 milliseconds for application quit hooks.
System quit saving is therefore best-effort cleanup, not a durability guarantee for slow storage.

## Persistence and lifecycle

A change notification does not mean that a disk write succeeded.
Wait for the save result before reporting success or releasing the workspace.
If saving fails, retain the component so the user can retry.

Save only committed editor content.
`EditorView::committed_document()` excludes an input method's uncommitted candidate.
`EditorView::doc()` can contain that candidate during composition.

Preserve the source-aware editing and persistence path when composing a custom editor.
Do not replace that path with a normalized Markdown serializer after each edit.
The source tracker preserves content and syntax that the visual editor does not replace.

Dropping an entity is not a save operation.
The host must complete the close operation before it releases the component or exits.
An application quit hook is a fallback, not the only close path for an embedded panel.

## GPUI dependency identity

Use the same GPUI package and version as the component.
This checkout pins `gpui-pre` and `gpui-pre-platform` to `0.3.6`.
Matching Rust type names from different GPUI packages do not make those types interchangeable.

Markraft also applies a local macOS patch to `gpui-pre-macos`.
Cargo reads patches from the final consumer's workspace root.
The external consumer manifest repeats that patch and points to the prepared source.
Run `python3 scripts/prepare-dependencies.py` before checking the example.

The example manifest disables stripping for build dependencies in development and release profiles.
This preserves loadable procedural macro libraries with the Xcode 27 tools used for local verification.

For a separate repository, provide a reproducible patch preparation step and update the patch path in the consumer manifest.
See [the patch instructions](../patches/README.md) for the pinned archive, checksum, and build requirements.

## macOS App Sandbox

The `sandbox` feature is disabled by default.
Enable it when the host executable runs in the App Sandbox.
The workspace then does not read the data of other applications.
For example, it does not list Obsidian vaults.

The Markraft application also stores folder grants for files from outside the notes folder.
That interface is not yet available to a host.
The host must keep its own security-scoped bookmarks for each directory and file that it gives to the workspace.

## Optional macOS translation

The `native-translation` feature is disabled by default.
The external workspace example uses that default.
If the final host enables native translation, its executable must weak-link the Translation framework and locate the Swift runtime.
Library build-script linker arguments do not propagate to the final executable.

Add these arguments to the host executable's `build.rs`:

```rust
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-weak_framework,Translation");
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
}
```

Keep the feature disabled if the host does not provide this native capability.
Verify the resulting executable on the oldest macOS version the host supports.

## Interface compatibility

The public interface of `markraft-notes` is `NotesLibrary`, `NotesBackend`, and the types that they use.
The public interface of `markraft-workspace` is `WorkspaceView`, its options, and its events.
The store, the storage worker, and the settings file have no compatibility promise.
A host cannot name them without the `unstable-internals` feature of `markraft-notes`.
The `unstable-standalone` feature of `markraft-workspace` names what only the Markraft application uses.
Do not enable these two features in a host.

`scripts/check-public-api.sh` compares both public interfaces with the latest release.
It uses `cargo-semver-checks`, and it runs on each pull request.
A change that breaks a host fails the check.
To release such a change, add a change file that declares the release `major`.
A new value of an option, such as a new line width, is such a change.
A new preference, event, or receipt field is not.

## Verification boundaries

The consumer check builds and links the complete workspace host.
It runs the notes service example with Markdown and SQLite storage.
It also tests SQLite persistence, stale writes, rollback, tombstones, history, dirty acknowledgements, and attachment identity.
It also rejects GPUI and application dependencies in the notes consumer's dependency tree.
It also confirms that the notes consumer does not enable `unstable-internals`.

Compilation does not verify native input, focus, window behavior, or file-access grants.
Test those behaviors in the actual host application.
Include input methods, undo, paste, save failures, and closing during pending work.
If the host embeds multiple workspaces, verify that their settings and close operations remain independent.
