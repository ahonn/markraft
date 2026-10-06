# GPUI macOS compatibility patch

Markraft keeps a diff against the published `gpui-pre-macos` 0.3.6 package.
The repository does not include a full copy of that package.
The root `[patch.crates-io]` selects the generated source for both distribution channels.
No GPUI public Rust interfaces change.

## Prepare dependencies

Requirements: Python 3.9 or newer, Git, and network access for the first download.

Before the first Cargo command, run:

```sh
python3 scripts/prepare-dependencies.py
```

Run this command again after changing branches or updating the patch.
The script verifies the archive SHA-256 before extraction and applies the patch with `git apply`.
The generated crate and download cache reside in the ignored `.build/dependencies` directory.
This directory survives `cargo clean`.
The script checks the generated source against its recorded hash and rebuilds stale or modified files.
An unchanged checkout reuses the prepared source without network access.

GitHub Actions runs this script before Cargo.
A fresh checkout cannot build without preparation because Cargo requires the generated path.
Cargo does not fall back to the unpatched registry crate when that path is missing.
Cargo cannot apply a diff through `[patch]`, and a Rust build script runs too late to prepare dependency manifests.

## Provenance

- Package: [gpui-pre-macos 0.3.6](https://crates.io/crates/gpui-pre-macos/0.3.6)
- Archive SHA-256: `951d17e41a72067ad2d35c60e08a62e8ad7fa9400b41c802e26cfb459f6c05da`
- Upstream: [Zed](https://github.com/zed-industries/zed)
- Snapshot revision from package metadata: `bcf6582ce3500df93a8a39366640173e6786cea6`
- License: [Apache-2.0](LICENSE-APACHE). The downloaded source retains its license and notices.
- Patch: [gpui-pre-macos-0.3.6.patch](gpui-pre-macos-0.3.6.patch)

## Changes to upstream

Only `src/window.rs` differs from the published source.

| Original behavior | Replacement and tradeoff |
| --- | --- |
| Private `_opaqueRectForWindowMoveWhenInTitlebar` override | Public `NSView.mouseDownCanMoveWindow` returns false for app-owned titlebars and defers to NSView otherwise. Explicit `performWindowDragWithEvent:` remains available. Suppression of the macOS 27 titlebar click delay is not guaranteed. |
| Private `_windowRestorationOptions` override and `NSWindowRestorationOptions` lookup | Use standard `NSKeyedUnarchiver` and public `restoreStateWithCoder:`. Remove the undocumented `NSWindowRestoresWorkspaceAtLaunch` workaround. AppKit controls Space placement. Restoration no longer forces the previous Space. |
| Private `_zoomFill:` | Public `zoom:` toggles window size. It does not reproduce system tiling margins. |
| Private diagonal resize cursors | Use public `frameResizeCursorFromPosition:inDirections:` on macOS 15+, guarded before invocation. macOS 13 and 14 use the public crosshair cursor. |
| Traversal of private `CAChameleonLayer` and saturation filters | Keep AppKit's native visual effect material. AppKit controls desktop tinting and saturation, so translucent windows can look different. |

The removed diagonal selectors are `_windowResizeNorthWestSouthEastCursor` and `_windowResizeNorthEastSouthWestCursor`.
The source audit inspected Objective-C class lookups, selectors, and direct framework imports in this crate.
This targeted audit does not certify all transitive dependencies or platform behavior.

Public API references:

- [NSView.mouseDownCanMoveWindow](https://developer.apple.com/documentation/appkit/nsview/mousedowncanmovewindow)
- [NSWindow.zoom](https://developer.apple.com/documentation/appkit/nswindow/zoom(_:))
- [NSCursor](https://developer.apple.com/documentation/appkit/nscursor/)

## Update the patch

1. Compare the new registry source with the current patch.
2. Port only the changes that upstream still needs.
3. Update the version, archive checksum, and patch path in `scripts/prepare-dependencies.py`.
4. Update this document and the pinned GPUI dependencies in `Cargo.toml`.
5. Run the preparation script before Cargo checks.

When upstream removes these private APIs, remove this patch and its build hooks.
Restore the registry dependency in `Cargo.lock`.

After a patch change, run the affected application checks and build a fresh binary.
Scan the binary for the removed selectors, `NSWindowRestorationOptions`, `NSWindowRestoresWorkspaceAtLaunch`, and `CAChameleonLayer`.
Source scanning alone does not verify the binary.
`CAFilter` occurred only in an upstream comment, not in a class lookup.

Test titlebar clicks, dragging, double-click zoom, native state restoration, resize cursors, and translucent windows.
Minimum-version and Intel behavior require suitable hardware.
Record executed checks in [the validation report](../docs/mac-app-store-validation.md).
