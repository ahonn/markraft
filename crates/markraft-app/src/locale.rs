//! Application translations. Preferences are persisted; resolved locale values
//! are immutable snapshots passed to each surface, never process-global state.
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::sync::LazyLock;

/// A presentation message retained as data until a view chooses its language.
/// Literal values are reserved for user content and external diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    Literal(String),
    Template {
        key: &'static str,
        args: Vec<(&'static str, Message)>,
    },
    Joined(Vec<Message>, &'static str),
}

impl Message {
    pub fn new(key: &'static str) -> Self {
        Self::Template {
            key,
            args: Vec::new(),
        }
    }

    pub fn is_key(&self, expected: &str) -> bool {
        matches!(self, Self::Template { key, .. } if *key == expected)
    }

    pub fn arg(mut self, name: &'static str, value: impl Into<Message>) -> Self {
        if let Self::Template { args, .. } = &mut self {
            args.push((name, value.into()));
        }
        self
    }

    pub fn join(messages: Vec<Message>, separator: &'static str) -> Self {
        Self::Joined(messages, separator)
    }

    pub fn render(&self, i18n: &I18n) -> String {
        match self {
            Self::Literal(text) => text.clone(),
            Self::Template { key, args } => {
                let values: Vec<_> = args
                    .iter()
                    .map(|(name, value)| (*name, value.render(i18n)))
                    .collect();
                let args: Vec<_> = values
                    .iter()
                    .map(|(name, value)| (*name, value.as_str()))
                    .collect();
                i18n.text_with(key, &args)
            }
            Self::Joined(messages, separator) => messages
                .iter()
                .map(|message| message.render(i18n))
                .collect::<Vec<_>>()
                .join(separator),
        }
    }
}

impl std::fmt::Display for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.render(&I18n::english()))
    }
}

impl From<String> for Message {
    fn from(text: String) -> Self {
        Self::Literal(text)
    }
}

impl From<&str> for Message {
    fn from(text: &str) -> Self {
        Self::Literal(text.to_owned())
    }
}

impl From<&Message> for Message {
    fn from(message: &Message) -> Self {
        message.clone()
    }
}

mod catalog {
    // The macro embeds every resource at compile time and initializes its lookup
    // tables once. Do not set a default locale: that would mutate global state.
    rust_i18n::i18n!("locales", fallback = "en");

    pub(super) fn text(locale: &str, key: &str) -> String {
        _rust_i18n_try_translate(locale, key)
            .map(std::borrow::Cow::into_owned)
            .unwrap_or_else(|| key.to_owned())
    }
}

#[cfg(test)]
mod fixtures {
    rust_i18n::i18n!("tests/locale-fixtures", fallback = "en");

    pub(super) fn text(locale: &str, key: &str) -> String {
        _rust_i18n_try_translate(locale, key)
            .map(std::borrow::Cow::into_owned)
            .unwrap_or_else(|| super::catalog::text("en", key))
    }
}

/// Persist the request rather than its current resolution: a language missing
/// from this version can become available in a later update without losing it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum LanguagePreference {
    #[default]
    System,
    Locale(String),
}

impl Serialize for LanguagePreference {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(match self {
            Self::System => "system",
            Self::Locale(locale) => locale,
        })
    }
}

impl<'de> Deserialize<'de> for LanguagePreference {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match String::deserialize(deserializer)?.as_str() {
            "system" => Self::System,
            locale => Self::Locale(locale.to_owned()),
        })
    }
}

#[derive(Debug, Deserialize)]
pub struct Language {
    pub id: String,
    /// Native display name; language choices must remain readable in any locale.
    pub name: String,
}

/// Shared with the packaging tool, which emits CFBundleLocalizations from this
/// same file. English is first so Foundation uses it when no preference matches.
pub fn available_languages() -> &'static [Language] {
    static LANGUAGES: LazyLock<Vec<Language>> = LazyLock::new(|| {
        serde_json::from_str(include_str!("../locale_catalog.json"))
            .expect("the bundled language catalog is validated by tests")
    });
    &LANGUAGES
}

#[derive(Clone, Debug)]
pub struct I18n {
    locale: String,
    // An immutable compiled catalog, shared by all normal application instances.
    // Tests can use a separate catalog without registering fixture languages.
    catalog: fn(&str, &str) -> String,
}

impl I18n {
    pub fn editor_messages(&self) -> markraft_gpui::EditorMessages {
        let i18n = self.clone();
        markraft_gpui::EditorMessages::new(move |message, args| i18n.text_with(message.key(), args))
    }
    #[cfg(test)]
    pub(crate) fn fixture(locale: &str) -> Self {
        Self {
            locale: locale.to_owned(),
            catalog: fixtures::text,
        }
    }

    pub fn english() -> Self {
        Self {
            locale: "en".to_owned(),
            catalog: catalog::text,
        }
    }

    pub fn for_preference(preference: &LanguagePreference) -> Self {
        let supported: Vec<_> = available_languages()
            .iter()
            .map(|language| language.id.as_str())
            .collect();
        let locale = match preference {
            LanguagePreference::System => crate::platform::locale::preferred_language(&supported),
            LanguagePreference::Locale(requested) => {
                crate::platform::locale::resolve(&supported, Some(&[requested.as_str()]))
            }
        };
        Self {
            locale,
            catalog: catalog::text,
        }
    }

    pub fn locale(&self) -> &str {
        &self.locale
    }

    pub fn text(&self, key: &str) -> String {
        (self.catalog)(self.locale(), key)
    }

    /// Values are inserted once by rust-i18n, so user text containing `%{name}`
    /// stays literal instead of becoming another round of interpolation.
    pub fn text_with(&self, key: &str, args: &[(&str, &str)]) -> String {
        let (patterns, values): (Vec<_>, Vec<_>) = args
            .iter()
            .map(|(name, value)| (*name, (*value).to_owned()))
            .unzip();
        rust_i18n::replace_patterns(&self.text(key), &patterns, &values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::Path;

    #[test]
    fn editor_defaults_match_application_resources() {
        let english = I18n::english();
        for message in markraft_gpui::EditorMessage::ALL {
            assert_eq!(english.text(message.key()), message.english());
        }
    }

    #[test]
    fn retained_messages_render_with_the_current_language() {
        let message = Message::new("error.file-unavailable")
            .arg("name", "user %{detail}.md")
            .arg("detail", Message::new("error.untitled-note"));
        let english = I18n::english();
        let chinese = I18n::for_preference(&LanguagePreference::Locale("zh-Hant".into()));
        assert_ne!(message.render(&english), message.render(&chinese));
        assert!(message.render(&chinese).contains("user %{detail}.md"));
        assert!(
            message
                .render(&chinese)
                .contains(&chinese.text("error.untitled-note"))
        );
    }

    #[test]
    fn preference_round_trip_preserves_unknown_languages() {
        for value in ["system", "en", "zh-Hans", "zh-Hant", "future-Language"] {
            let json = serde_json::to_string(value).unwrap();
            let preference: LanguagePreference = serde_json::from_str(&json).unwrap();
            assert_eq!(serde_json::to_string(&preference).unwrap(), json);
        }
        assert_eq!(LanguagePreference::default(), LanguagePreference::System);
        let preference = LanguagePreference::Locale("future-Language".into());
        assert_eq!(I18n::for_preference(&preference).locale(), "en");
        assert_eq!(
            preference,
            LanguagePreference::Locale("future-Language".into())
        );
    }

    #[test]
    fn production_preferences_resolve_registered_language_regions() {
        for (requested, expected) in [
            ("en", "en"),
            ("en-AU", "en"),
            ("zh-Hans", "zh-Hans"),
            ("zh-CN", "zh-Hans"),
            ("zh-SG", "zh-Hans"),
            ("zh-Hans-CN", "zh-Hans"),
            ("zh-Hant", "zh-Hant"),
            ("zh-TW", "zh-Hant"),
            ("zh-HK", "zh-Hant"),
            ("zh-Hant-HK", "zh-Hant"),
        ] {
            let preference = LanguagePreference::Locale(requested.into());
            let i18n = I18n::for_preference(&preference);
            assert_eq!(i18n.locale(), expected, "{requested}");
            assert_eq!(preference, LanguagePreference::Locale(requested.into()));
        }
    }

    #[test]
    fn production_catalog_translates_registered_languages_independently() {
        let english = I18n::for_preference(&LanguagePreference::Locale("en".into()));
        let simplified = I18n::for_preference(&LanguagePreference::Locale("zh-Hans".into()));
        let traditional = I18n::for_preference(&LanguagePreference::Locale("zh-Hant".into()));

        assert_eq!(english.text("command.new-note"), "New Note");
        assert_eq!(simplified.text("command.new-note"), "新建笔记");
        assert_eq!(simplified.text("settings.language"), "语言");
        assert_eq!(
            simplified.text_with("notes.edited-days-ago", &[("days", "12")]),
            "12 天前编辑"
        );
        assert_eq!(traditional.text("command.new-note"), "新增筆記");
        assert_eq!(traditional.text("settings.language"), "語言");
        assert_eq!(
            traditional.text_with("notes.edited-days-ago", &[("days", "12")]),
            "12 天前編輯"
        );
        assert_eq!(
            english.text_with("notes.edited-days-ago", &[("days", "12")]),
            "Edited 12 days ago"
        );
        assert_eq!(english.text("command.new-note"), "New Note");
        assert_eq!(traditional.text("command.new-note"), "新增筆記");
    }

    #[test]
    fn instances_translate_independently_and_fall_back_per_message() {
        let english = I18n::fixture("en");
        let chinese = I18n::fixture("zh-Hans");
        assert_eq!(
            english.text_with("greeting", &[("name", "Ada")]),
            "Hello, Ada"
        );
        assert_eq!(
            chinese.text_with("greeting", &[("name", "Ada")]),
            "你好，Ada"
        );
        assert_eq!(chinese.text("only-english"), "English fallback");
        assert_eq!(chinese.text("unknown-key"), "unknown-key");
        assert_eq!(
            english.text_with("greeting", &[("name", "Ada")]),
            "Hello, Ada"
        );
        assert_eq!(
            english.text_with("greeting", &[("name", "%{name}")]),
            "Hello, %{name}"
        );
    }

    fn read_messages(directory: &Path, messages: &mut BTreeMap<String, BTreeMap<String, String>>) {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                read_messages(&path, messages);
                continue;
            }
            assert_eq!(path.extension().unwrap(), "json", "{}", path.display());
            let locale = path.file_stem().unwrap().to_str().unwrap().to_owned();
            let value: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            let object = value.as_object().unwrap();
            assert_eq!(
                object.get("_version"),
                Some(&serde_json::json!(1)),
                "{}",
                path.display()
            );
            let catalog = messages.entry(locale).or_default();
            for (key, value) in object {
                if key == "_version" {
                    continue;
                }
                flatten_messages(key, value, catalog);
            }
        }
    }

    fn flatten_messages(
        key: &str,
        value: &serde_json::Value,
        catalog: &mut BTreeMap<String, String>,
    ) {
        match value {
            serde_json::Value::String(text) => {
                assert!(
                    catalog.insert(key.into(), text.clone()).is_none(),
                    "duplicate key: {key}"
                );
            }
            serde_json::Value::Object(children) => {
                for (child, value) in children {
                    flatten_messages(&format!("{key}.{child}"), value, catalog);
                }
            }
            _ => panic!("message {key} must be a string or a namespace"),
        }
    }

    fn placeholders(text: &str) -> BTreeSet<&str> {
        text.split("%{")
            .skip(1)
            .map(|part| {
                let (name, _) = part
                    .split_once('}')
                    .expect("unclosed interpolation parameter");
                assert!(!name.is_empty(), "empty interpolation parameter");
                name
            })
            .collect()
    }

    #[test]
    fn registered_resources_have_unique_keys_and_matching_parameters() {
        let languages = available_languages();
        assert_eq!(
            languages.first().unwrap().id,
            "en",
            "English must be the fallback"
        );
        let ids: BTreeSet<_> = languages
            .iter()
            .map(|language| language.id.as_str())
            .collect();
        assert_eq!(ids.len(), languages.len(), "duplicate language identifier");
        assert!(
            languages
                .iter()
                .all(|language| !language.name.trim().is_empty())
        );
        let mut messages = BTreeMap::new();
        read_messages(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("locales")
                .as_path(),
            &mut messages,
        );
        assert_eq!(
            messages.keys().map(String::as_str).collect::<BTreeSet<_>>(),
            ids
        );
        let english = &messages["en"];
        for (locale, translations) in &messages {
            for (key, text) in translations {
                let source = english
                    .get(key)
                    .unwrap_or_else(|| panic!("unknown key {locale}:{key}"));
                assert_eq!(placeholders(text), placeholders(source), "{locale}:{key}");
                assert_eq!(
                    catalog::text(locale, key),
                    *text,
                    "compiled resource {locale}:{key}"
                );
            }
        }
    }
}
