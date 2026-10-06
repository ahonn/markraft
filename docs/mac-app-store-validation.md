# Mac App Store compatibility

The application has a separate Mac App Store channel with App Sandbox enabled.
The default channel retains Sparkle updates.
Xcode Cloud published `0.1.7 (2)` to internal TestFlight on 2026-10-06.
Installation and the smoke tests below passed on an Apple Silicon Mac.
These checks do not constitute App Store approval or qualification for every supported device.

## Behavior

- Single-instance requests use a bounded file queue in the system temporary directory.
  Directory permissions are 0700, and file permissions are 0600.
  A filesystem watcher wakes the queue without scanning the disk during idle polling.
  This avoids forbidden `/tmp` writes and Unix socket path limits.
- User-selected directories have app-scoped security bookmarks in `settings.bookmarks.json`.
  Startup restores permissions before the note store and file watchers access external files.
  Active scopes remain open for the application state lifetime.
  Restoration handles moved directories, stale bookmarks, aliases, and damaged catalogs.
- Independent files require a containing-directory grant for atomic saves.
  The application explains the required directory before opening the folder selection panel.
  Cancelling stops the requested open operation.
  This authorization does not change the notes root.
  Saved directory grants prevent repeated prompts after a restart.
- Store builds omit Sparkle, update menus, and update preferences.
  Selecting both distribution features, or neither feature, causes a compile error.
- Store Obsidian export uses a selected vault and validates its `.obsidian` directory.
  Store builds exclude automatic discovery through Obsidian's private configuration.
- The maintained GPUI patch replaces the identified private AppKit APIs.
  The patch applies to both channels.
  [Patch documentation](../patches/README.md) records provenance, license, and behavior differences.

The existing atomic-save protocol creates a temporary file beside the destination.
A file-only bookmark does not authorize that operation.
Native FileManager replacement APIs could reduce the required directory access in a future change.
Those APIs have not been verified as a replacement for this application's conflict and durability protocol.

## Build and package

The commands require macOS and the repository's Rust toolchain.
Run `python3 scripts/prepare-dependencies.py` before the Cargo commands below.
On the validation host, Xcode 27 rejects some proc-macro dylibs without `RUSTFLAGS='-C strip=none'`.
The flag is a local workaround for [Rust issue 157750](https://github.com/rust-lang/rust/issues/157750).
Repository toolchain and profile settings remain unchanged.

1. Build the sandboxed store bundle:

   ```sh
   RUSTFLAGS='-C strip=none' cargo xtask bundle --mac-app-store
   ```

   The output is `target/mac-app-store/debug/Markraft.app`.
   The command does not download or package Sparkle.
   The default signature is ad hoc and supports local validation only.
   `MARKRAFT_MAS_SIGN_IDENTITY` selects a configured signing identity.
   The command does not supply provisioning or an upload package.

2. Create a bundle with a separate validation identity:

   ```sh
   python3 scripts/prepare-mas-validation.py a-new-label
   ```

   The script reads the debug binary and refuses to overwrite an existing experiment directory.
   The output uses `app.markraft.masvalidation` and a separate application container.
   The binary report records its hash, linked libraries, selected private API strings, and signature verification.

3. Open the generated `Markraft MAS Validation.app` in `target/mas-validation/a-new-label/`.

4. Select a generated test directory in Settings > Files.

5. Edit a note and verify the saved bytes on disk.

6. Quit through the application menu.

7. Open the same signed bundle again.

   The note must remain readable and writable without another directory selection.
   An independent file outside that directory requires its containing-directory authorization once.

`cargo xtask bundle --mac-app-store --prebuilt` packages an existing store binary.
The command rejects a binary that still links Sparkle.
Use `--release` for optimized output.
A successful debug bundle does not qualify an optimized or universal release.

## TestFlight verification on 2026-10-06

Xcode Cloud build 2 published `0.1.7 (2)` from commit `c14ba9f`.
Apple processing reached `VALID`, and internal testing reached `IN_BETA_TESTING`.
The Cloud checks passed 2,035 Rust tests and 19 Python tests.
Four existing Rust tests were ignored.
The archive passed checks for both architectures, signing, sandbox entitlements, and matching debug symbols.

The Apple TestFlight app installed this build on an Apple Silicon Mac running macOS 27.0.
The installed bundle uses `app.markraft.mac` and contains an App Store receipt.
Strict signature verification passed, the sandbox entitlements were present, and Sparkle was absent.
The TestFlight app installed at `/Applications/Markraft 2.app` beside the existing DMG app.

| Device check | Result |
| --- | --- |
| Markdown and Chinese rendering | Headings, lists, emphasis, inline code, and Chinese text rendered. |
| Folder authorization | The system folder panel granted access to a generated notes directory. |
| Autosave | New notes and pasted English and Chinese text saved the expected bytes. |
| Undo and redo | Undo removed a pasted marker from disk, and redo restored it. |
| Search and file watching | Search opened a second note. An external file addition appeared without restarting. |
| HTML export | The exported HTML contained the expected note content. |
| Images | A local image and an HTTPS image rendered. |
| Process restart | The app restored the note and wrote a new note without another folder prompt. |
| Folder command | The command palette opened the folder picker. Cancellation preserved the current folder. |
| Independent file | A parent-directory grant permitted atomic saves outside the notes directory. |

No application crash was observed during these checks.
The main test sequence used generated fixtures and did not edit existing notes.
One preliminary test note was created in the app's initial directory.
The detailed local report and installed-build metadata are in `target/testflight-validation/`.
These generated artifacts are not committed.

Automated Chinese typing dropped characters in both Markraft and TextEdit.
This result does not isolate an application defect.
Chinese clipboard paste, rendering, and saved bytes passed.
Actual IME composition and candidate selection still need manual verification.
This pass does not cover Intel hardware, other macOS versions, PDF export, printing, image clipboard paste, or Finder's Open With command.

## Merge preparation verification on 2026-10-06

The final local checks covered both distribution channels and the release hooks.

| Check | Result |
| --- | --- |
| Default workspace Clippy and tests | Passed. Tests: 2,036 passed, 4 existing ignored. |
| Store workspace Clippy and tests | Passed. Tests: 2,036 passed, 4 existing ignored. |
| Python hook and archive tests | 28 passed. |
| Sparkle release-feed integration test | 1 passed, explicitly run with `--ignored`. |
| Formatting and `git diff --check` | Passed. |
| Direct-distribution debug bundle | Signature and plist checks passed. |
| Store optimized universal archive | Build 43 passed signature, sandbox, architecture, debug-symbol, and encryption-declaration checks. |

The direct-distribution bundle used the real public `SPARKLE_PUBLIC_KEY`.
Sparkle checked the release feed and reported that `0.1.7` was the newest version.
The app launched with a generated notes directory.
Titlebar dragging changed the saved window origin from `(1679, 1022)` to `(1200, 830)`.
A normal quit and restart restored the note and saved window bounds.

Reopening the running app through Finder displayed the note.
Settings and the translucent window rendered correctly.
Double-clicking the titlebar did not produce an observed size change, so zoom remains unverified.
Spaces behavior was not tested.
These checks used a debug bundle and do not qualify an optimized universal release.

A separate local Xcode archive used the optimized `app-store` profile and ad-hoc signing.
It contains both architectures and the boolean `ITSAppUsesNonExemptEncryption=false` declaration.
This archive was not uploaded, and build number 43 was not reserved in App Store Connect.

The first launch against the existing `~/Documents/Markraft` directory blocked during startup.
A process sample showed `Store::open` waiting in `read_dir` and `__open_nocancel`.
The generated directory opened normally.
The cause was not isolated, and these observations do not establish that this change introduced the blocked operation.
This change does not claim to fix it.

After testing, the original DMG settings were restored byte for byte.
The debug app was closed, and the TestFlight app was restored.
The original app bundle and existing notes were not changed.

An API read confirmed that `Store Archive` enables the `v`-prefix tag trigger with `autoCancel=false` and retains manual branch builds.
The first real release tag and an upgrade through TestFlight still require verification.

## Rebase verification on 2026-10-06

The branch was rebased onto `origin/master` at `efdcafd2c988d2c3c2390fd698e560a6b344e480`.
The application version is 0.1.7.
The store bundler now installs the native icon resources and metadata produced by the upstream icon compiler.

| Check | Result |
| --- | --- |
| Formatting and Clippy for both channels | Passed. |
| Workspace tests | 2,031 passed, 0 failed, 4 ignored. |
| Store application tests | 504 passed, 0 failed, 2 ignored. |
| Store packaging and strict signature verification | Passed. |
| Compiled icon | `Assets.car` is present and `CFBundleIconName` is `Markraft`. |

Logs are in `target/rebase-validation/`.
Native sandbox interaction checks were not repeated during this rebase.

## Initial migration verification

The initial migration used `origin/master`, commit `165bd2c4d80fc105fd3e7e7888c3c2c8c76e25a1`.
That migration contained only the App Store changes and their checks.
That migration did not include the separate native-editor commits from the original validation worktree.

Host: arm64, macOS 27.0 (26A428), Xcode 27.0 (27A266a), Rust 1.95.0.
Validation date: 2026-10-03.
All Rust compilation commands below used the local `RUSTFLAGS` workaround.

| Check | Result |
| --- | --- |
| `cargo fmt --all -- --check` | Passed. |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | Passed. |
| Store-channel Clippy with all targets and `-D warnings` | Passed. |
| Store application tests | 408 passed, 0 failed, 2 existing ignored. |
| Workspace tests | 1842 passed, 0 failed, 4 ignored across test binaries and doctests. |
| Sparkle release-feed integration test | 1 passed, explicitly run with `--ignored`. |
| Store bundle and isolated validation bundle | Built and passed strict signature verification. |
| Binary scan | No Sparkle link and none of the eight identified private API strings. |

The dependency patch still emits upstream deprecation warnings.
Workspace Clippy passes with `-D warnings` because the patched crate is an external dependency.
CI checks formatting, the default workspace, and the store application channel.

Local migration logs are in `target/migration-validation/`.
The isolated binary report is in `target/mas-validation/master-migration/binary-report.json`.
These generated files are not committed.

## Earlier sandbox validation

The original validation worktree included uncommitted native-editor changes based on `f519e3c`.
Its test counts differ from the remote-master migration because the source trees differ.
The original fixed tree passed 472 store application tests, with 2 existing ignored tests.

That tree passed these native sandbox checks with generated fixtures:

- A selected external notes directory remained readable and writable after a normal quit and restart.
- A renamed directory restored its notes and updated the saved paths.
- An independent file restored after restart.
  The initial file-only grant failed during atomic save.
  The containing-directory fix then passed two saves across a normal restart without another prompt.
  The notes root remained unchanged.
- A second launch forwarded an absolute-path request to the existing process.
- The store application menu and About settings omitted update controls.
- The editor and translucent window rendered after the GPUI patch.

These earlier checks were not all repeated on the migrated branch.
The TestFlight section records the subset tested again on the distributed build.
No actual user note directory was granted to the test application.
The initial migration repeated compilation, tests, packaging, signature verification, and the binary scan.

## Remaining release requirements

- Verify the first release tag starts Xcode Cloud automatically and reaches the internal TestFlight group.
  Compare the DMG and TestFlight source commits and marketing versions.
  Test an upgrade from `0.1.7 (2)` through TestFlight.
- Complete App Review requirements before public distribution.
  Cloud signing, upload, Apple processing, and internal TestFlight installation have passed.
- Provide a published privacy policy, an in-app policy link, privacy answers, screenshots, and review metadata.
- Run the optimized universal release on supported macOS versions and Intel hardware.
  Universal archiving passed, but the Intel executable was not run on Intel hardware.
- Verify actual IME composition, PDF export, image clipboard paste, and Finder file opening.
- Check provider and iCloud files, login items, accessibility, Spaces behavior, and physical printing.
- Design authorization for attachments outside all selected directories.
  A reference in a note does not grant access to an arbitrary external file.
- Translate the remaining English bookmark-recovery diagnostics.

Historical logs contain `RefCell already borrowed` errors around window reactivation.
Their cause was not isolated, and this change does not claim to fix them.

Relevant Apple references:
[App Review Guidelines](https://developer.apple.com/app-store/review/guidelines/)
and [sandbox file access](https://developer.apple.com/documentation/security/accessing-files-from-the-macos-app-sandbox).
