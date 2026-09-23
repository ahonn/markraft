//! The house style: the delimiters Markraft writes for syntax it adds.
//!
//! Source a writer typed or a file brought keeps its own spelling; the house
//! style only decides what a formatting command, or content spelled from its
//! marks — pasted HTML — writes where there was no spelling before. Today that
//! is the delimiter emphasis and strong are written with: `*a*` and `**a**`,
//! or `_a_` and `__a__`.
//!
//! `_` cannot open or close inside a word — CommonMark reads `foo_bar_baz` as
//! plain text — so wherever the `_` spelling would not be read as the style,
//! the `*` spelling is written instead.

use std::cell::Cell;

/// The spelling choices Markraft makes for new syntax.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HouseStyle {
    /// The delimiter character for emphasis and strong: `'*'` or `'_'`.
    pub emphasis: char,
}

impl Default for HouseStyle {
    fn default() -> HouseStyle {
        HouseStyle { emphasis: '*' }
    }
}

// Thread-local rather than process-wide: the editor reads and writes
// documents on one thread, so a thread-local reaches every command without
// threading a setting through each call, while tests — which the harness runs
// on threads of their own — can each set a style without racing the others.
thread_local! {
    static HOUSE_STYLE: Cell<HouseStyle> = Cell::new(HouseStyle::default());
}

/// Make `style` the house style on this thread.
///
/// An emphasis delimiter other than `'*'` or `'_'` is taken as `'*'`.
pub fn set_house_style(style: HouseStyle) {
    debug_assert!(
        matches!(style.emphasis, '*' | '_'),
        "emphasis is written with `*` or `_`, not {:?}",
        style.emphasis
    );
    let emphasis = if style.emphasis == '_' { '_' } else { '*' };
    HOUSE_STYLE.with(|cell| cell.set(HouseStyle { emphasis }));
}

/// The house style on this thread.
pub fn house_style() -> HouseStyle {
    HOUSE_STYLE.with(Cell::get)
}

/// The emphasis delimiters worth trying for new syntax, best first: the house
/// one, then `*` where the house one is `_` and cannot be read.
pub(crate) fn emphasis_candidates() -> &'static [char] {
    if house_style().emphasis == '_' {
        &['_', '*']
    } else {
        &['*']
    }
}
