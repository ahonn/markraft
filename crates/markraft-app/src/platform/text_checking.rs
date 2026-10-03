//! Native text checking with explicit document lifetimes and scalar-indexed results.

use crate::locale::Message;
use objc2::{MainThreadMarker, class, msg_send, rc::Retained, runtime::AnyObject};
use objc2_foundation::{
    NSArray, NSDataDetector, NSDictionary, NSGrammarCorrections, NSGrammarRange,
    NSGrammarUserDescription, NSMatchingOptions, NSRange, NSString, NSTextCheckingResult,
    NSTextCheckingType,
};
use std::{ops::Range, ptr};

const DATA_DETECTOR_TYPES: u64 = NSTextCheckingType::Date.0
    | NSTextCheckingType::Address.0
    | NSTextCheckingType::PhoneNumber.0
    | NSTextCheckingType::TransitInformation.0;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CheckOptions {
    pub spelling: bool,
    pub grammar: bool,
    pub quotes: bool,
    pub dashes: bool,
    pub replacements: bool,
    pub correction: bool,
    pub links: bool,
    pub data_detectors: bool,
}

impl CheckOptions {
    pub(super) fn mask(self) -> u64 {
        [
            (self.spelling, NSTextCheckingType::Spelling),
            (self.grammar, NSTextCheckingType::Grammar),
            (self.quotes, NSTextCheckingType::Quote),
            (self.dashes, NSTextCheckingType::Dash),
            (self.replacements, NSTextCheckingType::Replacement),
            (self.correction, NSTextCheckingType::Correction),
            (self.links, NSTextCheckingType::Link),
            (self.data_detectors, NSTextCheckingType::Date),
            (self.data_detectors, NSTextCheckingType::Address),
            (self.data_detectors, NSTextCheckingType::PhoneNumber),
            (self.data_detectors, NSTextCheckingType::TransitInformation),
        ]
        .into_iter()
        .filter(|(enabled, _)| *enabled)
        .fold(0, |mask, (_, kind)| mask | kind.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CheckKind {
    Spelling,
    Grammar,
    Quote,
    Dash,
    Replacement,
    Correction,
    Link,
    Date,
    Address,
    Phone,
    Transit,
}

/// An immutable detector result together with the exact string it checked.
/// The native object includes metadata required by AppKit's data actions.
#[derive(Clone)]
pub(crate) struct DetectedData {
    pub(super) result: Retained<NSTextCheckingResult>,
    pub(super) text: Retained<NSString>,
}

impl std::fmt::Debug for DetectedData {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DetectedData")
            .field("range", &self.result.range())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct TextCheck {
    pub kind: CheckKind,
    /// Unicode scalar offsets, never UTF-8 bytes or UTF-16 code units.
    pub range: Range<usize>,
    pub replacement: Option<String>,
    pub suggestions: Vec<String>,
    pub detail: Option<String>,
    pub url: Option<String>,
    /// Preserve the original detector result for the system's data menu.
    pub data: Option<DetectedData>,
}

/// Keep one instance per editor document so ignored words do not leak between notes.
pub(crate) struct SpellDocument {
    checker: Retained<AnyObject>,
    detector: Retained<NSDataDetector>,
    tag: isize,
}

impl SpellDocument {
    pub(crate) fn new() -> Result<Self, Message> {
        MainThreadMarker::new().ok_or_else(failure)?;
        let checker: Option<Retained<AnyObject>> =
            unsafe { msg_send![class!(NSSpellChecker), sharedSpellChecker] };
        let detector = NSDataDetector::dataDetectorWithTypes_error(DATA_DETECTOR_TYPES)
            .map_err(|_| failure())?;
        let checker = checker.ok_or_else(failure)?;
        let tag = unsafe { msg_send![class!(NSSpellChecker), uniqueSpellDocumentTag] };
        Ok(Self {
            checker,
            detector,
            tag,
        })
    }

    /// Check the provided prose snapshot. Callers should exclude code/markup and
    /// keep this bounded to a paragraph or explicit user-requested selection.
    pub(crate) fn check(&self, text: &str, options: CheckOptions) -> Vec<TextCheck> {
        if MainThreadMarker::new().is_none() || text.is_empty() || options.mask() == 0 {
            return Vec::new();
        }
        let string = NSString::from_str(text);
        let native_range = NSRange::new(0, text.encode_utf16().count());
        let mut results = Vec::new();
        let checking_types = options.mask() & !DATA_DETECTOR_TYPES;
        if checking_types != 0 {
            let checked: Retained<NSArray<NSTextCheckingResult>> = unsafe {
                msg_send![&*self.checker,
                    checkString: &*string,
                    range: native_range,
                    types: checking_types,
                    options: ptr::null::<AnyObject>(),
                    inSpellDocumentWithTag: self.tag,
                    orthography: ptr::null_mut::<*mut AnyObject>(),
                    wordCount: ptr::null_mut::<isize>()]
            };
            results.extend(checked.iter());
        }
        if options.data_detectors {
            // NSSpellChecker omits phone results on some macOS versions even
            // when requested. NSDataDetector supplies every supported data kind
            // and preserves the native metadata that AppKit's menus require.
            let detected = self.detector.matchesInString_options_range(
                &string,
                NSMatchingOptions::empty(),
                native_range,
            );
            results.extend(detected.iter());
        }
        let mut checks = Vec::new();
        for result in results {
            let Some(kind) = check_kind(result.resultType()) else {
                continue;
            };
            let native_range = result.range();
            let Some(range) = utf16_to_scalar_range(text, native_range) else {
                continue;
            };
            if kind == CheckKind::Grammar {
                // Grammar result ranges cover a sentence; each detail identifies
                // the actual questionable phrase relative to that sentence.
                if let Some(details) = result.grammarDetails() {
                    for detail in details.iter() {
                        let local: NSRange = unsafe { detail.objectForKey(NSGrammarRange) }
                            .map(|value| unsafe { msg_send![&*value, rangeValue] })
                            .unwrap_or(NSRange::new(0, native_range.length));
                        let Some(absolute) = nested_range(native_range, local) else {
                            continue;
                        };
                        let Some(range) = utf16_to_scalar_range(text, absolute) else {
                            continue;
                        };
                        let suggestions: Option<Retained<NSArray<NSString>>> =
                            unsafe { msg_send![&*detail, objectForKey: NSGrammarCorrections] };
                        let description: Option<Retained<NSString>> =
                            unsafe { msg_send![&*detail, objectForKey: NSGrammarUserDescription] };
                        checks.push(TextCheck {
                            kind,
                            range,
                            replacement: None,
                            suggestions: suggestions
                                .map(|items| items.iter().map(|item| item.to_string()).collect())
                                .unwrap_or_default(),
                            detail: description.map(|value| value.to_string()),
                            url: None,
                            data: None,
                        });
                    }
                }
                continue;
            }
            let suggestions = if kind == CheckKind::Spelling {
                self.guesses_native(&string, native_range)
            } else {
                Vec::new()
            };
            let replacement = match kind {
                CheckKind::Quote
                | CheckKind::Dash
                | CheckKind::Replacement
                | CheckKind::Correction => {
                    result.replacementString().map(|value| value.to_string())
                }
                _ => None,
            };
            let url = if kind == CheckKind::Link {
                result
                    .URL()
                    .and_then(|url| url.absoluteString())
                    .map(|value| value.to_string())
            } else {
                None
            };
            let data = matches!(
                kind,
                CheckKind::Date | CheckKind::Address | CheckKind::Phone | CheckKind::Transit
            )
            .then(|| DetectedData {
                result: result.clone(),
                text: string.clone(),
            });
            checks.push(TextCheck {
                kind,
                range,
                replacement,
                suggestions,
                detail: None,
                url,
                data,
            });
        }
        checks.sort_by_key(|check| (check.range.start, check.range.end));
        checks
    }

    pub(crate) fn tag(&self) -> isize {
        self.tag
    }

    pub(crate) fn panel_visible(&self, panel: super::checking_panel::CheckingPanel) -> bool {
        use super::checking_panel::CheckingPanel;
        unsafe {
            let panel: Retained<AnyObject> = match panel {
                CheckingPanel::Spelling => msg_send![&*self.checker, spellingPanel],
                CheckingPanel::Substitutions => msg_send![&*self.checker, substitutionsPanel],
            };
            msg_send![&*panel, isVisible]
        }
    }

    pub(crate) fn hide_panel(&self, panel: super::checking_panel::CheckingPanel) {
        use super::checking_panel::CheckingPanel;
        unsafe {
            let panel: Retained<AnyObject> = match panel {
                CheckingPanel::Spelling => msg_send![&*self.checker, spellingPanel],
                CheckingPanel::Substitutions => msg_send![&*self.checker, substitutionsPanel],
            };
            let _: () = msg_send![&*panel, orderOut: ptr::null::<AnyObject>()];
        }
    }

    pub(crate) fn show_panel(&self, panel: super::checking_panel::CheckingPanel) {
        use super::checking_panel::CheckingPanel;
        unsafe {
            let panel: Retained<AnyObject> = match panel {
                CheckingPanel::Spelling => msg_send![&*self.checker, spellingPanel],
                CheckingPanel::Substitutions => msg_send![&*self.checker, substitutionsPanel],
            };
            let _: () = msg_send![&*self.checker, updatePanels];
            let _: () = msg_send![&*panel, makeKeyAndOrderFront: ptr::null::<AnyObject>()];
        }
    }

    pub(crate) fn update_panel(&self, word: &str) {
        unsafe {
            let _: () = msg_send![&*self.checker, updateSpellingPanelWithMisspelledWord: &*NSString::from_str(word)];
            let _: () = msg_send![&*self.checker, updatePanels];
        }
    }

    pub(crate) fn update_grammar_panel(
        &self,
        phrase: &str,
        description: &str,
        suggestions: &[String],
    ) {
        let text = NSString::from_str(phrase);
        let description = NSString::from_str(description);
        let suggestions = NSArray::from_retained_slice(
            &suggestions
                .iter()
                .map(|text| NSString::from_str(text))
                .collect::<Vec<_>>(),
        );
        unsafe {
            let range: Retained<AnyObject> = msg_send![class!(NSValue), valueWithRange: NSRange::new(0, phrase.encode_utf16().count())];
            let detail = NSDictionary::<NSString, AnyObject>::from_slices(
                &[
                    NSGrammarRange,
                    NSGrammarUserDescription,
                    NSGrammarCorrections,
                ],
                &[&*range, &*description, &*suggestions],
            );
            let _: () = msg_send![&*self.checker, updateSpellingPanelWithGrammarString: &*text, detail: &*detail];
        }
    }

    fn guesses_native(&self, text: &NSString, range: NSRange) -> Vec<String> {
        let guesses: Option<Retained<NSArray<NSString>>> = unsafe {
            msg_send![&*self.checker, guessesForWordRange: range, inString: text,
                language: ptr::null::<NSString>(), inSpellDocumentWithTag: self.tag]
        };
        guesses
            .map(|items| items.iter().map(|item| item.to_string()).collect())
            .unwrap_or_default()
    }

    pub(crate) fn ignore(&self, word: &str) {
        if MainThreadMarker::new().is_none() || word.is_empty() {
            return;
        }
        unsafe {
            let _: () = msg_send![&*self.checker, ignoreWord: &*NSString::from_str(word), inSpellDocumentWithTag: self.tag];
        }
    }

    pub(crate) fn learn(&self, word: &str) {
        if MainThreadMarker::new().is_none() || word.is_empty() {
            return;
        }
        unsafe {
            let _: () = msg_send![&*self.checker, learnWord: &*NSString::from_str(word)];
        }
    }

    pub(crate) fn has_learned(&self, word: &str) -> bool {
        if MainThreadMarker::new().is_none() || word.is_empty() {
            return false;
        }
        unsafe { msg_send![&*self.checker, hasLearnedWord: &*NSString::from_str(word)] }
    }

    pub(crate) fn unlearn(&self, word: &str) {
        if MainThreadMarker::new().is_none() || word.is_empty() {
            return;
        }
        unsafe {
            let _: () = msg_send![&*self.checker, unlearnWord: &*NSString::from_str(word)];
        }
    }
}

impl Drop for SpellDocument {
    fn drop(&mut self) {
        if MainThreadMarker::new().is_some() {
            unsafe {
                let _: () = msg_send![&*self.checker, closeSpellDocumentWithTag: self.tag];
            }
        }
    }
}

fn check_kind(value: NSTextCheckingType) -> Option<CheckKind> {
    match value {
        NSTextCheckingType::Spelling => Some(CheckKind::Spelling),
        NSTextCheckingType::Grammar => Some(CheckKind::Grammar),
        NSTextCheckingType::Quote => Some(CheckKind::Quote),
        NSTextCheckingType::Dash => Some(CheckKind::Dash),
        NSTextCheckingType::Replacement => Some(CheckKind::Replacement),
        NSTextCheckingType::Correction => Some(CheckKind::Correction),
        NSTextCheckingType::Link => Some(CheckKind::Link),
        NSTextCheckingType::Date => Some(CheckKind::Date),
        NSTextCheckingType::Address => Some(CheckKind::Address),
        NSTextCheckingType::PhoneNumber => Some(CheckKind::Phone),
        NSTextCheckingType::TransitInformation => Some(CheckKind::Transit),
        _ => None,
    }
}

fn nested_range(parent: NSRange, child: NSRange) -> Option<NSRange> {
    if child.location.checked_add(child.length)? > parent.length {
        return None;
    }
    Some(NSRange::new(
        parent.location.checked_add(child.location)?,
        child.length,
    ))
}

/// Reject ranges splitting a surrogate pair, out-of-bounds locations and NSNotFound.
fn utf16_to_scalar_range(text: &str, range: NSRange) -> Option<Range<usize>> {
    let end = range.location.checked_add(range.length)?;
    let mut start_scalar = (range.location == 0).then_some(0);
    let mut end_scalar = (end == 0).then_some(0);
    let mut utf16 = 0;
    for (index, character) in text.chars().enumerate() {
        utf16 += character.len_utf16();
        if utf16 == range.location {
            start_scalar = Some(index + 1);
        }
        if utf16 == end {
            end_scalar = Some(index + 1);
        }
    }
    Some(start_scalar?..end_scalar?)
}

fn failure() -> Message {
    Message::new("error.native-window-control")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_utf16_without_confusing_bytes_or_graphemes() {
        let text = "a😀中文e\u{301}";
        assert_eq!(utf16_to_scalar_range(text, NSRange::new(1, 2)), Some(1..2));
        assert_eq!(utf16_to_scalar_range(text, NSRange::new(3, 2)), Some(2..4));
        assert_eq!(utf16_to_scalar_range(text, NSRange::new(5, 2)), Some(4..6));
        assert_eq!(utf16_to_scalar_range(text, NSRange::new(7, 0)), Some(6..6));
        assert_eq!(utf16_to_scalar_range("", NSRange::new(0, 0)), Some(0..0));
    }

    #[test]
    fn rejects_invalid_utf16_boundaries_and_overflow() {
        let text = "a😀z";
        assert_eq!(utf16_to_scalar_range(text, NSRange::new(2, 1)), None);
        assert_eq!(utf16_to_scalar_range(text, NSRange::new(1, 1)), None);
        assert_eq!(utf16_to_scalar_range(text, NSRange::new(4, 1)), None);
        assert_eq!(
            utf16_to_scalar_range(text, NSRange::new(usize::MAX, 0)),
            None
        );
        assert_eq!(
            utf16_to_scalar_range(text, NSRange::new(usize::MAX, 1)),
            None
        );
    }

    #[test]
    fn grammar_details_are_relative_to_the_parent_sentence() {
        assert_eq!(
            nested_range(NSRange::new(5, 10), NSRange::new(2, 3)),
            Some(NSRange::new(7, 3))
        );
        assert_eq!(nested_range(NSRange::new(5, 10), NSRange::new(9, 2)), None);
        assert_eq!(
            nested_range(NSRange::new(usize::MAX, 10), NSRange::new(2, 3)),
            None
        );
    }
    #[test]
    fn data_detection_mask_covers_all_supported_contextual_kinds() {
        assert_eq!(CheckOptions::default().mask(), 0);
        assert_eq!(
            CheckOptions {
                data_detectors: true,
                ..Default::default()
            }
            .mask(),
            DATA_DETECTOR_TYPES
        );
        assert_eq!(
            CheckOptions {
                links: true,
                data_detectors: true,
                ..Default::default()
            }
            .mask(),
            DATA_DETECTOR_TYPES | NSTextCheckingType::Link.0
        );
    }

    #[test]
    fn real_data_detector_ranges_map_dates_addresses_phones_and_flights() {
        let detector = NSDataDetector::dataDetectorWithTypes_error(DATA_DETECTOR_TYPES)
            .expect("system data detector");
        for (text, native_kind, kind, expected) in [
            (
                "😀 Tomorrow at 3pm",
                NSTextCheckingType::Date,
                CheckKind::Date,
                2..17,
            ),
            (
                "😀 1 Apple Park Way, Cupertino, CA 95014",
                NSTextCheckingType::Address,
                CheckKind::Address,
                2..39,
            ),
            (
                "😀 +1 (415) 555-0123",
                NSTextCheckingType::PhoneNumber,
                CheckKind::Phone,
                2..19,
            ),
            (
                "😀 Flight AA 123",
                NSTextCheckingType::TransitInformation,
                CheckKind::Transit,
                9..15,
            ),
        ] {
            let string = NSString::from_str(text);
            let results = detector.matchesInString_options_range(
                &string,
                NSMatchingOptions::empty(),
                NSRange::new(0, text.encode_utf16().count()),
            );
            let result = results
                .iter()
                .find(|result| result.resultType() == native_kind)
                .expect("real detected result");
            assert_eq!(check_kind(result.resultType()), Some(kind));
            assert_eq!(utf16_to_scalar_range(text, result.range()), Some(expected));
            // Retain the original object and original input together; never
            // rebuild a synthetic result merely from its visible fields.
            let snapshot = DetectedData {
                result: result.clone(),
                text: string.clone(),
            };
            assert_eq!(
                Retained::as_ptr(&snapshot.result),
                Retained::as_ptr(&result)
            );
            assert_eq!(snapshot.text.to_string(), text);
        }
    }
}
