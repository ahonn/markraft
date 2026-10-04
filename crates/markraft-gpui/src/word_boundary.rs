//! Platform word selection in display-scalar coordinates, independent of
//! document syntax, layout direction and pointer geometry.
use std::ops::Range;

#[derive(Clone, Copy)]
pub(crate) enum Intent {
    Context,
    Pointer,
}

#[cfg(target_os = "macos")]
pub(crate) fn at(text: &str, character: usize, intent: Intent) -> Option<Range<usize>> {
    use objc2_app_kit::NSAttributedStringAppKitAdditions;
    use objc2_foundation::{NSAttributedString, NSString};

    let boundaries = text
        .chars()
        .scan(0, |utf16, character| {
            let start = *utf16;
            *utf16 += character.len_utf16();
            Some(start)
        })
        .chain([text.encode_utf16().count()])
        .collect::<Vec<_>>();
    let location = *boundaries.get(character)?;
    if location >= *boundaries.last()? {
        return None;
    }
    // Contextual selection uses AppKit's attributed-string word operation.
    // Pointer selection follows the distinct TextKit 2 navigation contract.
    let native = NSString::from_str(text);
    let range = match intent {
        Intent::Context => NSAttributedString::from_nsstring(&native).doubleClickAtIndex(location),
        Intent::Pointer => pointer_range(&native, location)?,
    };
    let end = range.location.checked_add(range.length)?;
    let from = boundaries.binary_search(&range.location).ok()?;
    let to = boundaries.binary_search(&end).ok()?;
    (from < to).then_some(from..to)
}

#[cfg(target_os = "macos")]
fn pointer_range(
    text: &objc2_foundation::NSString,
    location: usize,
) -> Option<objc2_foundation::NSRange> {
    use objc2::{
        class, msg_send,
        rc::{Allocated, Retained},
        runtime::AnyObject,
    };
    use objc2_foundation::{NSArray, NSRange};

    // TextEdit's TextKit 2 double-click navigation has different dictionary
    // boundaries from its contextual selection (for example 编辑器 versus 编辑).
    // These content/layout objects have no UI-thread-only view. Keep the whole
    // graph local to this call: it has no container, display, delegate or shared
    // mutable state, and never triggers layout or participates in focus.
    unsafe {
        let content: Retained<AnyObject> = msg_send![class!(NSTextContentStorage), new];
        let manager: Retained<AnyObject> = msg_send![class!(NSTextLayoutManager), new];
        let _: () = msg_send![&*content, addTextLayoutManager: &*manager];
        let alloc: Allocated<AnyObject> = msg_send![class!(NSTextStorage), alloc];
        let storage: Retained<AnyObject> = msg_send![alloc, initWithString: text];
        let _: () = msg_send![&*content, setTextStorage: &*storage];
        let document: Retained<AnyObject> = msg_send![&*content, documentRange];
        let start: Retained<AnyObject> = msg_send![&*document, location];
        let at: Option<Retained<AnyObject>> = msg_send![&*content, locationFromLocation: &*start, withOffset: isize::try_from(location).ok()?];
        let alloc: Allocated<AnyObject> = msg_send![class!(NSTextSelection), alloc];
        let selection: Retained<AnyObject> =
            msg_send![alloc, initWithLocation: &*at?, affinity: 1isize];
        let navigation: Retained<AnyObject> = msg_send![&*manager, textSelectionNavigation];
        let result: Retained<AnyObject> = msg_send![&*navigation, textSelectionForSelectionGranularity: 1isize, enclosingTextSelection: &*selection];
        let ranges: Retained<NSArray<AnyObject>> = msg_send![&*result, textRanges];
        if ranges.len() != 1 {
            return None;
        }
        let range = ranges.objectAtIndex(0);
        let from: Retained<AnyObject> = msg_send![&*range, location];
        let to: Retained<AnyObject> = msg_send![&*range, endLocation];
        let offset: isize = msg_send![&*content, offsetFromLocation: &*start, toLocation: &*from];
        let length: isize = msg_send![&*content, offsetFromLocation: &*from, toLocation: &*to];
        Some(NSRange::new(
            usize::try_from(offset).ok()?,
            usize::try_from(length).ok()?,
        ))
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn at(text: &str, character: usize, _: Intent) -> Option<Range<usize>> {
    use unicode_segmentation::UnicodeSegmentation;
    let mut start = 0;
    for word in text.split_word_bounds() {
        let end = start + word.chars().count();
        if (start..end).contains(&character) {
            return Some(start..end);
        }
        start = end;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{Intent, at};

    #[test]
    fn punctuation_spaces_and_graphemes_are_pointer_targets() {
        for (text, position, expected) in [
            ("one   two", 4, 3..6),
            ("one...two", 4, 4..5),
            ("a🦀b", 1, 1..2),
            ("a 👨‍👩‍👧‍👦 b", 5, 2..9),
            ("a e\u{301} b", 3, 2..4),
        ] {
            for intent in [Intent::Context, Intent::Pointer] {
                assert_eq!(at(text, position, intent), Some(expected.clone()), "{text}");
            }
        }
        assert_eq!(at("", 0, Intent::Pointer), None);
        assert_eq!(at("a", 1, Intent::Pointer), None);
        assert_eq!(at("🦀", usize::MAX, Intent::Pointer), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_dictionary_words_use_scalar_coordinates() {
        for position in 2..4 {
            assert_eq!(at("🦀 宇宙 洪荒", position, Intent::Context), Some(2..4));
        }
        assert_eq!(at("example.com", 7, Intent::Context), Some(0..11));
        assert_eq!(at("中文编辑器测试", 2, Intent::Context), Some(2..4));
        assert_eq!(at("中文编辑器测试", 2, Intent::Pointer), Some(2..5));
    }
}
