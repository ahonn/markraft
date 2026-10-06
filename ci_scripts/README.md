# Xcode Cloud builds for macOS

These hooks prepare and verify the `Markraft-Store` archive. The `Store Archive`
workflow is configured in Xcode Cloud for the Markraft App Store Connect app.
The release setup targets `v`-prefixed Git tags and retains manual builds.
The first automatic tag build still requires release verification.

## Workflow configuration

Use these settings when connecting the repository:

| Setting | Value |
| --- | --- |
| Project | `Markraft.xcodeproj` |
| Scheme | `Markraft-Store` |
| Platform and configuration | macOS, Release |
| Action | Archive, with App Store Connect distribution preparation |
| Start condition | Tags with the `v` prefix, plus manual builds |
| Post-action | Internal TestFlight testing, using the `Internal` group |
| Architectures | arm64 and x86_64 |
| Build image | Xcode 27 (27A266a), latest supported macOS release |

The app and workflow were registered on 2026-10-06 under team `C97A45J272`.

| Resource | Value |
| --- | --- |
| App | [Markraft](https://appstoreconnect.apple.com/apps/6819551024) |
| Bundle ID | `app.markraft.mac` |
| SKU and primary language | `MARKRAFT-MACOS-2026`, `en-US` |
| Cloud product | `9f586ff1-03f4-47ed-980e-2545c14bf6bb` |
| Workflow | `Store Archive`, `5D3A357D-4668-40E9-8955-CA99A9D14F9A` |
| Repository | `ahonn/markraft` |
| Implementation branch | `fix/mac-app-store-compatibility` |
| Internal testing group | `Internal`, `cad08346-45da-4b0d-bcb5-416ebf294b46` |

Xcode generated `Markraft.xcodeproj/xcshareddata/xcodecloud/manifest.json` when
connecting the project. Keep this file with the shared scheme.
The automatically created `Default` workflow is disabled.
Cloud build 2 completed archive, distribution signing, upload, and internal TestFlight distribution.
The `Internal` group contains the intended internal testers.

Installation through TestFlight and the local smoke tests passed on Apple Silicon with macOS 27.0.
The [device results](../docs/mac-app-store-validation.md#testflight-verification-on-2026-10-06) record the scope and limits.

The hooks use Apple's `CI_BUILD_NUMBER`, `CI_PRIMARY_REPOSITORY_PATH`, and archive
environment variables. They do not require an ASC API key, a Sparkle key, or
Developer ID credentials. Xcode Cloud manages the distribution signing setup.

## Hook responsibilities

| Hook | Action |
| --- | --- |
| `ci_post_clone.sh` | Install Rust, check formatting, run hook tests, and run workspace clippy and tests with the store feature |
| `ci_pre_xcodebuild.sh` | Restore the Rust environment and generate the Xcode build number include |
| `ci_post_xcodebuild.sh` | Verify each successful archive before the workflow can complete |

The three entry points reject execution outside Xcode Cloud. The preparation
hooks also reject a platform other than macOS or an invalid build number.
Run local archive builds with the commands in [the archive guide](../docs/xcode-archive.md).

The preparation hooks install rustup 1.28.2 from the official Rust distribution
server. They verify its SHA-256 digest against the pinned digest for the host
architecture. `rust-toolchain.toml` selects the compiler version, currently
1.95.0. Both macOS targets, rustfmt, and clippy are installed.

Both preparation hooks also download and apply the [pinned GPUI patch](../patches/README.md).
The local Xcode build phase uses the same script before Cargo starts.

Cargo and rustup use directories under `target/xcode-cloud/tools`.
The pre-build hook checks the installation again because later stages cannot rely
on shell exports or tools created by an earlier stage. The Xcode build phase
reconstructs the same paths. No hook changes the user's shell profile.

## Quality checks

The post-clone hook runs these checks before native archiving:

```sh
python3 scripts/prepare-dependencies.py
cargo fmt --all -- --check
python3 -m unittest discover -s ci_scripts/tests -v
cargo clippy --workspace --all-targets --locked \
  --no-default-features --features markraft-app/mac-app-store -- -D warnings
cargo test --workspace --locked \
  --no-default-features --features markraft-app/mac-app-store -- --test-threads=4
```

These commands select the store feature for the entire workspace check.
They do not download Sparkle. The existing GitHub CI workflow remains responsible
for its direct-distribution checks.

Tests use four threads. GPUI 0.3.6 test teardown retains visual contexts and their
file handles until the test process exits. This exceeds the initial 256-file
limit in some Cloud shells.
As a workaround, the hook raises the soft limit to at least 4,096 and logs the value.
It stops with a diagnostic if the host does not permit that limit.

The environment adds `-C strip=none` to `RUSTFLAGS` for the Xcode 27 Rust
proc-macro workaround. The archive uses the separate Cargo `app-store` profile
to preserve debug information.

## Release tags

The release setup uses Tag Changes with a `v` prefix for `Store Archive`.
Manual builds remain available, and ordinary branch pushes do not start an archive.
The existing GitHub release workflow builds the DMG, ZIP, and Sparkle update from the same release tag.
The two workflows run independently, so TestFlight does not wait for the DMG release to succeed.

For a tag build, the hooks require a stable `vX.Y.Z` tag that matches the Cargo package version.
Prerelease tags and mismatched versions must fail before archiving.
Manual builds use the Cargo version without a release tag.

The first real release tag still needs verification:

1. Confirm that the tag starts both release workflows.
2. Compare the source commit and marketing version in both channels.
3. Confirm that Apple processing completes without an export-compliance prompt.
4. Confirm that the new build reaches the internal TestFlight group.
5. Upgrade the installed `0.1.7 (2)` build through TestFlight.
6. Repeat the folder-access, editing, and autosave smoke tests after the upgrade.

## Build numbers

The pre-build hook writes `target/xcode-cloud/BuildNumber.xcconfig`.
The tracked `Markraft.xcodeproj/BuildNumber.xcconfig` includes this generated
file after the local default. The hook does not modify `project.pbxproj`.

The Xcode build phase checks that `CURRENT_PROJECT_VERSION` matches
`CI_BUILD_NUMBER`. The post-build hook checks the same number in both the archive
metadata and the app plist. The marketing version continues to come from Cargo.

Only the disposable Cloud checkout needs the generated include.
If you simulate the hooks locally, remove that generated file afterward to restore
the default local build number. The simulation does not reserve a build number
in App Store Connect.

## Archive verification

`verify_archive.py` uses Python 3.9 or newer and standard Apple command-line
tools. It resides entirely in `ci_scripts` and does not need the repository,
Cargo, or third-party Python packages during the post-build stage.

It checks the app identifier, release version, build number, application archive
structure, icon resources, and both executable architectures. It also checks the
signature, sandbox entitlements for both architectures, linked library paths,
dSYM UUIDs, and nonempty function and line information.
Store archives must set `ITSAppUsesNonExemptEncryption` to the Boolean value `false`.
The archive verifier checks this declaration before upload.

The verifier accepts the archive's signing identity, including local ad-hoc
signing. A valid archive signature does not prove that TestFlight distribution
signing or Apple processing has succeeded. Those are later workflow steps.

Failed Xcode actions and actions other than Archive skip the post-build checks.
This preserves the original build failure instead of reporting a missing archive.
A failed archive check returns a nonzero status with a diagnostic.

To check an existing local archive:

```sh
python3 ci_scripts/verify_archive.py \
  --archive target/xcode/Markraft.xcarchive \
  --build-number 1
```

The command prints a JSON summary when all checks pass.
It does not upload or modify the archive.

See Apple's [custom script documentation](https://developer.apple.com/documentation/xcode/writing-custom-build-scripts)
and [environment variable reference](https://developer.apple.com/documentation/xcode/environment-variable-reference)
for hook placement and stage availability.

## Local validation

On 2026-10-06, all three hooks passed a local simulation with Xcode 27.0.
The simulation installed the pinned Rust tools in an isolated directory.
Formatting, workspace clippy, and all 19 Python tests passed.
The store workspace tests passed 2,035 tests, with four existing tests ignored.

The first workspace run exposed a queue lock that could remain held during a
concurrent process launch. The queue now explicitly unlocks when its guard drops.
A regression test covers a duplicated handle that remains open after the guard drops.

The pre-build hook supplied build number `42` without a command-line version override.
The resulting archive kept version `0.1.7` and included both macOS architectures.
The post-build hook verified its metadata, signature, sandbox entitlements, icons,
linked libraries, and debug symbols. The post-build hook also passed without
access to the source checkout, using an earlier local archive.

This simulation used ad-hoc signing.
The separate Cloud release below verified remote execution, distribution signing, Apple processing, and installation through TestFlight.

## First TestFlight release

On 2026-10-06, Cloud build 2 published Markraft `0.1.7 (2)` from commit `c14ba9f`.
The `Store Archive` run completed with `SUCCEEDED` after about 12 minutes.
The build has Apple processing state `VALID` and internal state `IN_BETA_TESTING`.
The `Internal` group contains the build.

Cloud passed formatting, workspace clippy, 19 Python tests, and 2,035 Rust tests.
Four existing Rust tests were ignored. Archive verification confirmed both
architectures, the signature, sandbox entitlements, and debug symbols.
Build 1 failed before archiving because GPUI test contexts exhausted file descriptors.
The post-clone limit workaround resolved that failure in build 2.

The build initially required export compliance information. Remote images use
standard HTTPS through third-party `ureq`, `rustls`, and `ring` libraries.
The app does not implement proprietary encryption. This release is limited to
internal TestFlight testing and has no public App Store availability.

For this scope, the build's `usesNonExemptEncryption` value was set to `false`.
Apple cleared `MISSING_EXPORT_COMPLIANCE`. No encryption document was required
for the current scope. Reassess the declaration before distribution in the French
App Store, because the TLS implementation is not provided by the Apple OS.

Store bundles now include `ITSAppUsesNonExemptEncryption=false`, and archive verification checks the value.
This declaration covers the encryption behavior assessed for the current internal testing scope.
Reassess the declaration before public distribution, French availability, or a change to encryption behavior.
The first tag-triggered build must verify that Apple accepts the declaration without another prompt.

The build's English testing notes cover folder access after relaunch, editing,
autosave, Chinese input, images, settings, shortcuts, and opening files from Finder.
Publication and installation of this specific build are verified.
Local smoke tests passed for folder access after restart, autosave, undo and redo, search, HTML export, images, and external-file authorization.
Chinese rendering and clipboard paste passed. Actual IME composition remains unverified.
The [device results](../docs/mac-app-store-validation.md#testflight-verification-on-2026-10-06) list the remaining device checks.

[View Markraft in TestFlight](https://appstoreconnect.apple.com/apps/6819551024/testflight/macos).
