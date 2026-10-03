//! Context-sensitive text transformations and their menu capabilities.
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextTransformation {
    Uppercase,
    Lowercase,
    Capitalize,
    TraditionalChinese,
    SimplifiedChinese,
}

impl TextTransformation {
    /// AppKit offers all three case actions for cased letters, including letters
    /// with no case mapping. Chinese actions require a conversion that changes
    /// the selected text; they do not depend on a detected document language.
    pub fn available_for(text: &str) -> Vec<Self> {
        let mut available = Vec::new();
        if has_cased_letters(text) {
            available.extend([Self::Uppercase, Self::Lowercase, Self::Capitalize]);
        }
        for transformation in [Self::TraditionalChinese, Self::SimplifiedChinese] {
            if transformation
                .characters(text)
                .is_some_and(|characters| characters.concat() != text)
            {
                available.push(transformation);
            }
        }
        available
    }

    /// One replacement per original Unicode scalar, retaining its source marks.
    pub(super) fn characters(self, text: &str) -> Option<Vec<String>> {
        if matches!(self, Self::TraditionalChinese | Self::SimplifiedChinese) {
            let transformed = chinese(text, self)?;
            return aligned_characters(text, &transformed);
        }
        Some(case_characters(text, self))
    }
}

fn aligned_characters(original: &str, transformed: &str) -> Option<Vec<String>> {
    // The system's Chinese transforms substitute scalars without changing their
    // count. Reject a future incompatible result instead of guessing provenance.
    (original.chars().count() == transformed.chars().count()).then(|| {
        transformed
            .chars()
            .map(|character| character.to_string())
            .collect()
    })
}

#[cfg(target_os = "macos")]
fn has_cased_letters(text: &str) -> bool {
    use objc2_foundation::{NSCharacterSet, NSString};
    let text = NSString::from_str(text);
    [
        NSCharacterSet::uppercaseLetterCharacterSet(),
        NSCharacterSet::lowercaseLetterCharacterSet(),
    ]
    .iter()
    .any(|set| text.rangeOfCharacterFromSet(set).length > 0)
}

#[cfg(not(target_os = "macos"))]
fn has_cased_letters(text: &str) -> bool {
    text.chars()
        .any(|character| character.is_uppercase() || character.is_lowercase())
}

#[cfg(target_os = "macos")]
fn chinese(text: &str, transformation: TextTransformation) -> Option<String> {
    use objc2_foundation::NSString;
    let identifier = match transformation {
        TextTransformation::TraditionalChinese => "Simplified-Traditional",
        TextTransformation::SimplifiedChinese => "Traditional-Simplified",
        _ => return None,
    };
    // Transform the entire selection: converting isolated characters loses
    // context in phrases such as 头发 and 里面, even with applyTransform(range:).
    NSString::from_str(text)
        .stringByApplyingTransform_reverse(&NSString::from_str(identifier), false)
        .map(|text| text.to_string())
}

#[cfg(not(target_os = "macos"))]
fn chinese(_text: &str, _transformation: TextTransformation) -> Option<String> {
    None
}

fn case_characters(text: &str, transformation: TextTransformation) -> Vec<String> {
    let mut output = Vec::new();
    for segment in text.split_word_bounds() {
        let word = segment.unicode_words().next().is_some();
        // Lowercasing the complete word retains contextual Unicode mappings,
        // such as a Greek sigma at the end of a word.
        let lowercase = segment.to_lowercase();
        let mut lower = lowercase.chars();
        for (index, character) in segment.chars().enumerate() {
            let lower_count = character.to_lowercase().count();
            let lowered: String = lower.by_ref().take(lower_count).collect();
            output.push(match transformation {
                TextTransformation::Uppercase => character.to_uppercase().collect(),
                TextTransformation::Lowercase => lowered,
                TextTransformation::Capitalize if word && index == 0 => {
                    character.to_uppercase().collect()
                }
                TextTransformation::Capitalize if word => lowered,
                TextTransformation::Capitalize => character.to_string(),
                TextTransformation::TraditionalChinese | TextTransformation::SimplifiedChinese => {
                    unreachable!("Chinese conversion uses complete selection context")
                }
            });
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transformations_reject_ambiguous_scalar_provenance() {
        assert_eq!(
            aligned_characters("ab", "甲乙"),
            Some(vec!["甲".into(), "乙".into()])
        );
        assert_eq!(aligned_characters("a", "ab"), None);
        assert_eq!(aligned_characters("ab", "a"), None);
        assert_eq!(
            aligned_characters("😀a", "😀A"),
            Some(vec!["😀".into(), "A".into()])
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn transformations_match_native_contextual_menu_capabilities() {
        use TextTransformation::*;
        for text in ["abc", "ABC", "İı", "ΟΣ", "ß", "K", "ǅ", "ℝ", "𝔸", "ＡＢＣ"] {
            assert_eq!(
                TextTransformation::available_for(text),
                [Uppercase, Lowercase, Capitalize],
                "{text}"
            );
        }
        for text in [
            "中文",
            "123",
            "😀",
            "ひらがな",
            "カタカナ",
            "Ⅷ",
            "ⓐ",
            "ª",
            "里",
        ] {
            assert!(TextTransformation::available_for(text).is_empty(), "{text}");
        }
        assert_eq!(
            TextTransformation::available_for("编辑器"),
            [TraditionalChinese]
        );
        assert_eq!(
            TextTransformation::available_for("編輯器"),
            [SimplifiedChinese]
        );
        assert_eq!(
            TextTransformation::available_for("编辑器 編輯器"),
            [TraditionalChinese, SimplifiedChinese]
        );
        assert_eq!(
            TextTransformation::available_for("hello 编辑器"),
            [Uppercase, Lowercase, Capitalize, TraditionalChinese]
        );
        assert_eq!(
            TextTransformation::available_for("日本語"),
            [SimplifiedChinese]
        );
        assert_eq!(
            TraditionalChinese
                .characters("发头发干干净净后台 里面😀")
                .unwrap()
                .concat(),
            "發頭髮乾乾淨淨後台 裡面😀"
        );
        assert_eq!(
            SimplifiedChinese
                .characters("發頭髮乾乾淨淨後臺 裏面😀")
                .unwrap()
                .concat(),
            "发头发干干净净后台 里面😀"
        );
    }
}
