//! The house style: the delimiters Markraft writes for syntax it adds.
//!
//! Source a writer typed or a file brought keeps its own spelling; the house
//! style only decides what a formatting command, a key, or content spelled
//! from its marks — pasted HTML — writes where there was no spelling before:
//!
//! * the delimiter emphasis and strong are written with: `*a*` and `**a**`,
//!   or `_a_` and `__a__`;
//! * the delimiter after an ordered list's numbers, `1.` or `1)`, for a list
//!   made without a typed marker — see [`HouseStyle::ordered_delimiter`];
//! * how a new hard line break is spelled, a trailing `\` or two trailing
//!   spaces — see [`HardBreak`].
//!
//! `_` cannot open or close inside a word — CommonMark reads `foo_bar_baz` as
//! plain text — so wherever the `_` spelling would not be read as the style,
//! the `*` spelling is written instead.

use std::sync::{Arc, RwLock};

/// The spelling choices Markraft makes for new syntax.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HouseStyle {
    /// The delimiter character for emphasis and strong: `'*'` or `'_'`.
    pub emphasis: char,
    /// The delimiter after an ordered list's numbers: `'.'` or `')'`.
    ///
    /// It is the `delimiter` attribute of an `ordered_list` the editor makes
    /// without a typed marker — the list key, or an `<ol>` pasted from outside
    /// Markraft. A list typed as `1)` keeps its `)`, as one read from a file
    /// keeps whatever it was written with.
    pub ordered_delimiter: char,
    /// How a new hard line break is spelled.
    pub hard_break: HardBreak,
}

impl Default for HouseStyle {
    fn default() -> HouseStyle {
        HouseStyle {
            emphasis: '*',
            ordered_delimiter: '.',
            hard_break: HardBreak::Backslash,
        }
    }
}

/// The spelling of a hard line break the editor writes: Shift-Return, or a
/// `<br>` in pasted HTML. A break already in the source keeps its spelling.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum HardBreak {
    /// A backslash ending the line, `\` then the line ending: visible in the
    /// source, and kept by an editor that strips trailing whitespace.
    #[default]
    Backslash,
    /// Two spaces ending the line, then the line ending: invisible in the
    /// source, and the spelling every Markdown reader has always known.
    Spaces,
}

impl HardBreak {
    /// What is written before the line ending: `"\\"` or two spaces.
    pub fn marker(self) -> &'static str {
        match self {
            HardBreak::Backslash => "\\",
            HardBreak::Spaces => "  ",
        }
    }
}

impl HouseStyle {
    /// `self` with an emphasis delimiter other than `'*'` or `'_'` taken as
    /// `'*'`, and an ordered-list delimiter other than `'.'` or `')'` as `'.'`.
    fn normalised(self) -> HouseStyle {
        debug_assert!(
            matches!(self.emphasis, '*' | '_'),
            "emphasis is written with `*` or `_`, not {:?}",
            self.emphasis
        );
        debug_assert!(
            matches!(self.ordered_delimiter, '.' | ')'),
            "an ordered list is delimited with `.` or `)`, not {:?}",
            self.ordered_delimiter
        );
        HouseStyle {
            emphasis: if self.emphasis == '_' { '_' } else { '*' },
            ordered_delimiter: if self.ordered_delimiter == ')' {
                ')'
            } else {
                '.'
            },
            hard_break: self.hard_break,
        }
    }

    /// The emphasis delimiters worth trying for new syntax, best first: the
    /// house one, then `*` where the house one is `_` and cannot be read.
    pub(crate) fn emphasis_candidates(&self) -> &'static [char] {
        if self.emphasis == '_' {
            &['_', '*']
        } else {
            &['*']
        }
    }
}

/// The house style everything that spells new syntax shares.
///
/// A shared handle rather than a value: a host builds its codecs and
/// formatting commands once and they read the style when they write, so a
/// preference change reaches every editor without rebuilding them — and each
/// test builds a handle of its own, so none can race another. The default
/// handle holds the default style.
#[derive(Clone, Debug, Default)]
pub struct HouseStyleHandle(Arc<RwLock<HouseStyle>>);

impl HouseStyleHandle {
    /// A handle holding `style`.
    pub fn new(style: HouseStyle) -> HouseStyleHandle {
        HouseStyleHandle(Arc::new(RwLock::new(style.normalised())))
    }

    /// The style as it stands.
    pub fn get(&self) -> HouseStyle {
        *self
            .0
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Make `style` the house style for everything built over this handle.
    pub fn set(&self, style: HouseStyle) {
        *self
            .0
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = style.normalised();
    }
}
