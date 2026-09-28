//! Foundation owns language matching, including script and region variants.
use objc2_foundation::{NSArray, NSBundle, NSString};

/// Resolve the user's language for this application, including a macOS per-app
/// override. The packaged bundle declares the same supported languages.
pub(crate) fn preferred_language(supported: &[&str]) -> String {
    resolve(supported, None)
}

/// An explicit preference list also makes negotiation testable without changing
/// the user's language settings. Foundation uses the first supported language
/// when nothing matches, so the caller places its fallback first.
pub(crate) fn resolve(supported: &[&str], preferences: Option<&[&str]>) -> String {
    let array = |values: &[&str]| {
        NSArray::from_retained_slice(
            &values
                .iter()
                .map(|value| NSString::from_str(value))
                .collect::<Vec<_>>(),
        )
    };
    let supported = array(supported);
    let preferences = preferences.map(array);
    NSBundle::preferredLocalizationsFromArray_forPreferences(&supported, preferences.as_deref())
        .firstObject()
        .map(|language| language.to_string())
        .unwrap_or_else(|| "en".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_traditional_chinese_does_not_skip_supported_preferences() {
        assert_eq!(
            resolve(&["en", "zh-Hans"], Some(&["zh-Hant", "zh-Hans", "en"])),
            "zh-Hans"
        );
    }

    #[test]
    fn foundation_resolves_language_regions_and_scripts() {
        for (requested, expected) in [
            ("en-AU", "en"),
            ("zh-CN", "zh-Hans"),
            ("zh-SG", "zh-Hans"),
            ("zh-Hans-CN", "zh-Hans"),
            ("zh-TW", "en"),
            ("zh-Hant-HK", "en"),
            ("xx-Unknown", "en"),
        ] {
            assert_eq!(
                resolve(&["en", "zh-Hans"], Some(&[requested])),
                expected,
                "{requested}"
            );
        }
    }

    #[test]
    fn foundation_matches_traditional_chinese_regions_to_the_registered_script() {
        for requested in ["zh-Hant", "zh-TW", "zh-HK", "zh-Hant-HK"] {
            assert_eq!(
                resolve(&["en", "zh-Hant"], Some(&[requested])),
                "zh-Hant",
                "{requested}"
            );
        }
    }

    #[test]
    fn simplified_chinese_does_not_match_traditional_chinese_by_prefix() {
        for requested in ["zh-Hans", "zh-CN", "zh-SG", "zh-Hans-CN"] {
            assert_eq!(
                resolve(&["en", "zh-Hant"], Some(&[requested, "en"])),
                "en",
                "{requested}"
            );
        }
        assert_eq!(
            resolve(&["en", "zh-Hant"], Some(&["zh-Hans", "zh-Hant", "en"])),
            "zh-Hant"
        );
        assert_eq!(resolve(&["en", "zh-Hant"], Some(&["en", "zh-Hant"])), "en");
    }

    #[test]
    fn empty_preferences_use_the_declared_fallback() {
        assert_eq!(resolve(&["en", "zh-Hans"], Some(&[])), "en");
    }
}
