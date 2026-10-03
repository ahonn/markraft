//! AppKit decides word boundaries and whitespace; the editor maps the result
//! back to guarded source edits without giving a native text view the document.

use std::ops::Range;

pub(crate) struct Context {
    pub text: String,
    pub selection: Range<usize>,
    pub boundaries: Vec<usize>,
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use objc2::{MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{NSSelectionGranularity, NSTextView};
    use objc2_foundation::{NSRange, NSRect, NSString};

    fn view(text: &str) -> Option<objc2::rc::Retained<NSTextView>> {
        let mtm = MainThreadMarker::new()?;
        let view = NSTextView::initWithFrame(NSTextView::alloc(mtm), NSRect::ZERO);
        view.setString(&NSString::from_str(text));
        view.setSmartInsertDeleteEnabled(true);
        Some(view)
    }

    fn native_range(context: &Context) -> NSRange {
        let start = context
            .text
            .chars()
            .take(context.selection.start)
            .map(char::len_utf16)
            .sum();
        let length = context
            .text
            .chars()
            .skip(context.selection.start)
            .take(context.selection.len())
            .map(char::len_utf16)
            .sum();
        NSRange::new(start, length)
    }

    pub(crate) fn whole_words(context: &Context) -> bool {
        let Some(view) = view(&context.text) else {
            return false;
        };
        let range = native_range(context);
        range.length > 0
            && view.selectionRangeForProposedRange_granularity(
                range,
                NSSelectionGranularity::SelectByWord,
            ) == range
    }

    pub(crate) fn delete(context: &Context) -> Option<Range<usize>> {
        let view = view(&context.text)?;
        let range = view.smartDeleteRangeForProposedRange(native_range(context));
        scalar_range(
            &context.text,
            range.location..range.location.checked_add(range.length)?,
        )
    }

    pub(crate) fn padding(context: &Context, pasted: &str) -> Option<(String, String)> {
        let view = view(&context.text)?;
        let (mut before, mut after) = (None, None);
        view.smartInsertForString_replacingRange_beforeString_afterString(
            &NSString::from_str(pasted),
            native_range(context),
            Some(&mut before),
            Some(&mut after),
        );
        Some((
            before.map(|value| value.to_string()).unwrap_or_default(),
            after.map(|value| value.to_string()).unwrap_or_default(),
        ))
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::*;
    pub(crate) fn whole_words(_: &Context) -> bool {
        false
    }
    pub(crate) fn delete(_: &Context) -> Option<Range<usize>> {
        None
    }
    pub(crate) fn padding(_: &Context, _: &str) -> Option<(String, String)> {
        None
    }
}

pub(crate) use platform::{delete, padding, whole_words};

/// Native ranges must land on scalar boundaries, never inside a surrogate pair.
#[cfg(any(target_os = "macos", test))]
fn scalar_range(text: &str, range: Range<usize>) -> Option<Range<usize>> {
    let mut utf16 = 0;
    let mut from = None;
    let mut to = None;
    for (scalar, character) in text
        .chars()
        .map(Some)
        .chain(std::iter::once(None))
        .enumerate()
    {
        if utf16 == range.start {
            from = Some(scalar);
        }
        if utf16 == range.end {
            to = Some(scalar);
        }
        if let Some(character) = character {
            utf16 += character.len_utf16();
        }
    }
    Some(from?..to?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cocoa_ranges_preserve_unicode_and_reject_surrogate_interiors() {
        assert_eq!(scalar_range("a🦀 word", 4..8), Some(3..7));
        assert_eq!(scalar_range("a🦀 word", 2..8), None);
        assert_eq!(scalar_range("a🦀 word", 8..9), None);
    }
}
