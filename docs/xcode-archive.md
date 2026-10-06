# macOS store archives

`Markraft.xcodeproj` provides the `Markraft-Store` scheme for macOS builds and
archives. This target always selects the sandboxed `mac-app-store` channel.
It does not build a DMG or include Sparkle.

Cargo compiles the application. Xcode processes the final Info.plist, signs the
application, and creates the archive. The target contains no replacement main
function and does not link another executable over the Rust binary.

## Create a local archive

Requirements: a Mac, Xcode 26 or newer, Python 3.9 or newer, Git, and the Rust toolchain specified in `rust-toolchain.toml`.
Xcode 26 or newer is required for the Icon Composer icon.
The default Release archive contains both Apple Silicon and Intel code.

1. Install the Rust targets:

   ```sh
   rustup target add aarch64-apple-darwin x86_64-apple-darwin
   ```

2. Select the Xcode installation for this shell:

   ```sh
   export DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer
   ```

3. Create an archive with local ad-hoc signing:

   ```sh
   xcodebuild \
     -project Markraft.xcodeproj \
     -scheme Markraft-Store \
     -configuration Release \
     -destination 'generic/platform=macOS' \
     -derivedDataPath target/xcode/DerivedData \
     -archivePath target/xcode/Markraft.xcarchive \
     CURRENT_PROJECT_VERSION=1 \
     CODE_SIGN_IDENTITY=- \
     CODE_SIGN_STYLE=Manual \
     DEVELOPMENT_TEAM= \
     CODE_SIGN_INJECT_BASE_ENTITLEMENTS=NO \
     archive
   ```

4. Verify the archived signature:

   ```sh
   codesign --verify --deep --strict \
     target/xcode/Markraft.xcarchive/Products/Applications/Markraft.app
   ```

5. Compare the executable and dSYM UUIDs:

   ```sh
   xcrun dwarfdump --uuid \
     target/xcode/Markraft.xcarchive/Products/Applications/Markraft.app/Contents/MacOS/markraft-app
   xcrun dwarfdump --uuid \
     target/xcode/Markraft.xcarchive/dSYMs/Markraft.app.dSYM
   ```

Xcode reports `ARCHIVE SUCCEEDED`. The archive contains
`Products/Applications/Markraft.app` and `dSYMs/Markraft.app.dSYM`.
Each architecture must have the same UUID in the executable and dSYM.

This command does not install the app or contact App Store Connect.
An ad-hoc archive cannot be distributed through TestFlight.
For a local archive of one architecture, add `ARCHS=arm64` or `ARCHS=x86_64`.

## Build ownership

| Concern | Owner |
| --- | --- |
| Rust compiler version | `rust-toolchain.toml` |
| Rust dependencies | `Cargo.lock`, enforced with `--locked` |
| Release archive profile | Cargo `app-store`, with debug information and no stripping |
| Architectures | Xcode `ARCHS`, mapped to Rust targets |
| Rust build artifacts | `target/xcode-cargo`, separate from ordinary Cargo builds |
| App metadata and icon | Shared store assembly in `xtask/src/macos.rs` |
| Input plist and dSYM | `cargo xtask xcode-build` |
| Final plist, signature, archive | Xcode native application target |
| Sandbox entitlements | `crates/markraft-app/entitlements/mac-app-store.plist` |

The build phase runs each time so Cargo can check all Rust and resource inputs.
It prepares the [GPUI patch](../patches/README.md) before Cargo resolves dependencies.
Its declared outputs make Xcode wait before processing the plist and signing.
The build script disables Xcode's script sandbox because Cargo needs its toolchain,
dependency cache, and network access. The application's App Sandbox remains enabled.

The script preserves caller `RUSTFLAGS` and adds `-C strip=none`.
This avoids the Rust proc-macro corruption observed with the Xcode 27 strip tool.
The ordinary Cargo release profile and DMG commands remain unchanged.

## Identity and versions

The target uses Bundle ID `app.markraft.mac` and team `C97A45J272`.
The default signing style is Automatic. The command above overrides signing only
for local validation, without registering an identifier or obtaining a profile.

`CFBundleShortVersionString` comes from the Cargo package version.
`CFBundleVersion` comes from Xcode's `CURRENT_PROJECT_VERSION`, which defaults to
`1` for local builds. The build phase rejects a nonpositive or noninteger value.

For Xcode Cloud, `ci_pre_xcodebuild.sh` writes `CI_BUILD_NUMBER` to a generated
xcconfig include before Xcode starts the build. The build phase checks that
`CURRENT_PROJECT_VERSION` matches it whenever `CI_XCODE_CLOUD=TRUE`.

## Remote distribution setup

The [Cloud configuration](../ci_scripts/README.md) records the registered ASC app
and the `Store Archive` workflow. The hooks prepare tools and verify archives.
Cloud build 2 published `0.1.7 (2)` from commit `c14ba9f` on 2026-10-06.
Remote execution, distribution signing, upload, and Apple processing passed.
Installation through TestFlight and local smoke tests also passed on Apple Silicon with macOS 27.0.
The [device results](mac-app-store-validation.md#testflight-verification-on-2026-10-06) record the tested scope and remaining coverage.

The release setup targets Git tags with the `v` prefix and retains manual builds.
A tag build must use a stable `vX.Y.Z` tag that matches the Cargo version.
The first automatic tag build and upgrade through TestFlight still require release verification.

App Store signing and export require the registered identifier, matching
provisioning profile, and distribution identity. Xcode Cloud manages distribution
signing when the Archive action runs.

## Local validation

On 2026-10-06, Xcode 27.0 completed two Release archives with ad-hoc signing.
The first used build number `2`. The incremental archive used `3`.
Both kept marketing version `0.1.7` and contained arm64 and x86_64 code.

The archives contained the application and its matching dSYM.
Strict signature verification, sandbox entitlements, icon resources, and the
absence of Sparkle passed inspection. Both dSYM architectures contained function
locations and line information, with UUIDs matching the archived executable.

The xtask suite passed 21 tests, with one existing test ignored.
Clippy, formatting, project plist validation, and shared scheme XML validation
passed. The existing local store bundle command also passed regression validation.

These local checks used ad-hoc signing.
The separate Cloud release verified App Store export and TestFlight distribution.
The Intel executable was cross-compiled, not run on Intel hardware.
