//! The selection excerpt shared by Look Up and Translate.

use objc2::AllocAnyThread;
use objc2_foundation::{NSArray, NSCharacterSet, NSString};
use objc2_natural_language::{NLTagSchemeTokenType, NLTagger, NLTokenUnit};

pub(crate) fn selection_label(text: &str) -> String {
    // NSTextView replaces each line separator and NBSP independently. Tabs and
    // repeated interior spaces remain visible; split_whitespace loses them.
    let whitespace = NSCharacterSet::whitespaceAndNewlineCharacterSet();
    let text = NSString::from_str(text)
        .componentsSeparatedByCharactersInSet(&NSCharacterSet::newlineCharacterSet())
        .componentsJoinedByString(&NSString::from_str(" "))
        .stringByReplacingOccurrencesOfString_withString(
            &NSString::from_str("\u{a0}"),
            &NSString::from_str(" "),
        )
        .stringByTrimmingCharactersInSet(&whitespace);
    const LIMIT: usize = 30;
    if text.length() <= LIMIT {
        return text.to_string();
    }

    // NLTagger matches the native menu's linguistic boundaries, including
    // contractions ("do" / "n't"). NSString/NLTokenizer word enumeration does
    // not. A single token longer than the limit falls back to the length cap.
    let mut end = LIMIT;
    unsafe {
        if let Some(scheme) = NLTagSchemeTokenType {
            let tagger =
                NLTagger::initWithTagSchemes(NLTagger::alloc(), &NSArray::from_slice(&[scheme]));
            tagger.setString(Some(&text));
            let token = tagger.tokenRangeAtIndex_unit(LIMIT, NLTokenUnit::Word);
            if token.location > 0 && token.location < LIMIT {
                end = token.location;
            }
        }
    }
    // Keep valid Unicode even if the fallback cap lands inside a surrogate pair.
    let mut units = 0;
    let mut label: String = text
        .to_string()
        .chars()
        .take_while(|ch| {
            units += ch.len_utf16();
            units <= end
        })
        .collect();
    label = NSString::from_str(&label)
        .stringByTrimmingCharactersInSet(&whitespace)
        .to_string();
    label.push('…');
    label
}

#[cfg(test)]
mod tests {
    #[test]
    fn captions_match_native_text_view_reference() {
        let cases: Vec<(String, String)> =
            serde_json::from_str(include_str!("fixtures/captions.json")).unwrap();
        assert!(cases.len() >= 30);
        for (input, expected) in cases {
            assert_eq!(super::selection_label(&input), expected, "{input:?}");
        }
    }
}
