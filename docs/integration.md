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

Provide separate notes, state, and cache locations when constructing a workspace.
The notes directory contains Markdown files.
The state directory contains component recovery and workspace data.
The cache directory contains replaceable component data.
Keep host preferences separate from component state.

The integration examples pass explicit directories below a temporary root.
Production hosts must select persistent directories and retain access for the component's entire lifetime.
Sandboxed hosts must retain their own file-access grants while workers and readers use those directories.

## Use the notes service

`NotesLibrary::open(NotesConfig::new(notes_directory, state_directory))` opens a service without a GPUI application.
`create()` accepts Markdown and returns a `NoteId`.
It creates a semantic scratch document.
Use `create_file()` when importing exact Markdown source, including front matter and original whitespace.

`search()` returns lightweight `NoteSummary` values without document content.
`note()` returns a `NoteSnapshot` with Markdown and a structured document.
`edit()` requires the snapshot revision and rejects a stale revision.
Revisions belong to one open library lifetime and are not persistent version tokens.

Await `flush()` to receive a `SaveReceipt`.
The receipt distinguishes saved Markdown files from drafts kept in recovery storage.
An empty scratch note does not require an empty Markdown file.
Await `close()` before opening the same storage in another service.
If either operation fails, inspect the error and retain the service for recovery or retry.
The [notes consumer](../examples/integration/notes-cli/src/main.rs) shows the full sequence.

Startup and search are synchronous operations.
Use the host's background executor when a large directory could delay the interface.
The save and close futures do not require a particular asynchronous runtime.

## Embed the workspace

Call `bind_workspace_keys(cx)` once from the host's GPUI initialization.
Create a `WorkspaceView` entity with `WorkspaceView::open(options, window, cx)`.
Render that entity inside the host's layout.
Retain subscriptions to `WorkspaceEvent` while the host uses the workspace.
Handle `OptionsChanged` by saving the editor preferences in the host's own configuration.
The example writes `host-preferences.json` beside its temporary directories, outside the component state directory.

The [graphical consumer](../examples/integration/workspace-host/src/main.rs) provides host-owned save and close controls.
It also routes the native close button and the host's quit action through the component close operation.
Its settings and support handlers display host status messages.
They do not open Markraft application windows.

`flush(window, cx, callback)` saves the currently committed revision.
Later edits remain dirty.
The callback receives a result and the workspace's window and context.

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

## Verification boundaries

The consumer check builds and links the complete workspace host, then runs the notes service example.
It also rejects GPUI and application dependencies in the notes consumer's dependency tree.

Compilation does not verify native input, focus, window behavior, or file-access grants.
Test those behaviors in the actual host application.
Include input methods, undo, paste, save failures, and closing during pending work.
If the host embeds multiple workspaces, verify that their settings and close operations remain independent.
