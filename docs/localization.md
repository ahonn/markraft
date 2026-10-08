# Localization

Markraft embeds JSON translations with `rust-i18n`. The app owns an `I18n` value
and passes it explicitly to views. Shared crates do not own language preferences.
English (`en`) is the fallback language.

## Adding a language

1. Copy every `crates/markraft-notes/locales/{domain}/en.json` to
   `{domain}/{locale}.json` and translate the values.
2. Add the BCP 47 tag and native display name to
   `crates/markraft-notes/locale_catalog.json`, for example:
   `{ "id": "zh-Hans", "name": "简体中文" }`. Keep `en` first.
3. Run the checks below and verify the packaged app on macOS.

The registry supplies both the language picker and macOS bundle metadata.
No language-specific Rust branch is needed. Use separate script tags for
Simplified Chinese (`zh-Hans`) and Traditional Chinese (`zh-Hant`).

## Writing messages

Resources use version 1 JSON with semantic, domain-qualified keys:

```json
{
  "_version": 1,
  "settings": {
    "language": "Language",
    "version": "Version %{version}"
  }
}
```

Keep keys unique across files and preserve `%{parameter}` names. Translate whole
messages so arguments can be reordered. Missing translations fall back to English.

```rust
let label = i18n.text("settings.language");
let version_label = i18n.text_with("settings.version", &[("version", &version)]);
```

For notifications and stored errors, retain a `Message` and render it when shown:

```rust
let notice = Message::new("error.conflict-note")
    .arg("title", note.title_message());
let label = notice.render(&i18n);
```

Pass structured errors through without calling `to_string()` early:
`Message::Display` produces English for diagnostics. Literal message arguments
are for user content and external diagnostic details.

Translate labels, menus, tooltips, accessibility text, and application-owned error
explanations. Preserve user Markdown, custom titles, filenames, URLs, code
identifiers, shortcut names, and external diagnostics. Logs and CLI syntax stay
outside the UI catalog. Use `Note::display_title` or `Note::title_message` for
empty-note labels. Never translate persisted filenames.

Dates use translated templates and month labels. Counts use explicit
singular and plural keys. Interpolation does not provide general CLDR plural rules.
Regional date and number formatting is separate from UI language selection.

## Language changes

The saved preference is `"system"` or a language tag. Foundation matches it against
supported languages, including macOS per-app preferences. Preserve unknown saved
tags even when their effective language falls back to English.

Changing language must refresh views, menus, cached completion labels, and stored
messages while preserving input, documents, selection, and undo history. Avoid
process-global locale state and cached translated strings.

The shared editor exposes `EditorMessage` keys with English defaults. The app
injects translations through `EditorMessages`:

```rust
use markraft_workspace::EditorLocaleExt;

editor.with_messages(i18n.editor_messages());
editor.set_messages(i18n.editor_messages(), cx);
```

For new editor text, add its typed key and English default in `markraft-gpui`,
then add matching `editor.*` resources in `markraft-notes`. Generated labels are translated.
User-authored titles remain unchanged.

## Verification

```sh
cargo test -p markraft-notes locale --locked
cargo test -p markraft-workspace --features unstable-standalone language --locked
cargo test -p xtask macos::tests --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Tests check resource keys, parameters, fallback, language matching, and runtime
switching. Review JSON for duplicate properties and translations for consistent
terminology. See [Contributing](../README.md#contributing) for build prerequisites.

For macOS testing, build with `MARKRAFT_SIGN_IDENTITY=- cargo xtask bundle` and run
the bundled executable with `--dir` and `--settings` pointing to temporary paths.
Check Settings, native menus, long labels, search, errors, and generated editor
labels across language changes. Verify undo, restart persistence, and the macOS
per-app language setting.
