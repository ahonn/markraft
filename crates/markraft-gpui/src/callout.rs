//! What the view has to know about a callout, which is as little as possible.
//!
//! A callout is a block quote carrying a type and a title in attributes. Which
//! attributes those are is the host's to say, through
//! [`DocTypes::callout`](crate::DocTypes::callout): leave it unset and nothing
//! here finds a callout, whatever a quote's attributes are called. The view
//! reads three things off a quote that is one: whether it has a header at all,
//! what the header says, and which accent it and the quote's bar are drawn in.
//! The tone table below is the conventional grouping of callout types; it
//! only ever runs for a host that opted in by naming the attributes.
//!
//! The fold marker is deliberately *not* acted on: the content is always drawn.
//! A note whose body an editor hid would be a note whose body could not be
//! edited, and the marker's byte is preserved either way.

use markraft_core::kind::DocTypes;
use markraft_core::projection::{Ancestor, Line};

/// The colour family a callout is drawn in, following the conventional grouping.
///
/// [`EditorStyle::callout_tones`](crate::EditorStyle::callout_tones) holds one
/// accent per variant, in the order they are declared here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Tone {
    /// note, info, todo — and anything the table does not know.
    Note,
    /// abstract, summary, tldr, tip, hint, important.
    Summary,
    /// success, check, done.
    Success,
    /// question, help, faq, warning, caution, attention.
    Caution,
    /// failure, fail, missing, danger, error, bug.
    Danger,
    /// example.
    Example,
    /// quote, cite.
    Quote,
}

impl Tone {
    /// Its place in [`EditorStyle::callout_tones`](crate::EditorStyle::callout_tones).
    pub(crate) fn index(self) -> usize {
        match self {
            Tone::Note => 0,
            Tone::Summary => 1,
            Tone::Success => 2,
            Tone::Caution => 3,
            Tone::Danger => 4,
            Tone::Example => 5,
            Tone::Quote => 6,
        }
    }
}

/// The tone a callout type belongs to. A type matches without regard to case,
/// and one not known is drawn in the default style, a [`Tone::Note`].
pub(crate) fn tone_of(kind: &str) -> Tone {
    match kind.to_lowercase().as_str() {
        "abstract" | "summary" | "tldr" | "tip" | "hint" | "important" => Tone::Summary,
        "success" | "check" | "done" => Tone::Success,
        "question" | "help" | "faq" | "warning" | "caution" | "attention" => Tone::Caution,
        "failure" | "fail" | "missing" | "danger" | "error" | "bug" => Tone::Danger,
        "example" => Tone::Example,
        "quote" | "cite" => Tone::Quote,
        _ => Tone::Note,
    }
}

/// A callout's header: what it reads as, and the tone it is drawn in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Head {
    pub(crate) label: String,
    pub(crate) tone: Tone,
}

/// The header a callout's type and title spell: the title where it has one, and
/// the localized default for a known type where it has not.
fn localized_head(kind: &str, title: &str, messages: &crate::EditorMessages) -> Option<Head> {
    if kind.is_empty() {
        return None;
    }
    let label = match title.trim() {
        "" => messages.callout_title(kind),
        title => title.to_owned(),
    };
    Some(Head {
        label,
        tone: tone_of(kind),
    })
}

/// A type as a header reads it: `note` becomes `Note`, and a type an author
/// capitalised keeps the capitals it was given.
#[cfg(test)]
fn head_of(kind: &str, title: &str) -> Option<Head> {
    localized_head(kind, title, &crate::EditorMessages::ENGLISH)
}

/// The innermost block quote a line sits in, when it is a callout that has this
/// line as its first — which is the line the header is drawn above.
pub(crate) fn header_of(
    types: &DocTypes,
    line: &Line,
    previous: Option<&Line>,
    messages: &crate::EditorMessages,
) -> Option<Head> {
    let quote = innermost_quote(types, line)?;
    // The header belongs to the quote's own first line. Every line of the
    // quote's first block passes `opens_quote`, so the line above settles it:
    // the first one has no line of the same quote before it.
    let continues = previous
        .and_then(|previous| innermost_quote_before(types, previous))
        .is_some_and(|above| Some(above) == innermost_quote_before(types, line));
    if continues || !opens_quote(types, line) {
        return None;
    }
    let attrs = types.callout?;
    localized_head(attr(quote, attrs.kind), attr(quote, attrs.title), messages)
}

/// The tone of every block quote a line sits in, outermost first, for the bars
/// drawn beside it — `None` for an ordinary quote. Each bar keeps its own
/// callout's tone, so an outer callout still reads as itself beside a nested one.
pub(crate) fn tones_beside(types: &DocTypes, line: &Line) -> Vec<Option<Tone>> {
    line.ancestors()
        .iter()
        .filter(|ancestor| Some(ancestor.node_type) == types.blockquote)
        .map(|quote| {
            let kind = types
                .callout
                .map(|attrs| attr(quote, attrs.kind))
                .unwrap_or_default();
            (!kind.is_empty()).then(|| tone_of(kind))
        })
        .collect()
}

fn innermost_quote<'a>(types: &DocTypes, line: &'a Line) -> Option<&'a Ancestor> {
    line.ancestors()
        .iter()
        .rev()
        .find(|ancestor| Some(ancestor.node_type) == types.blockquote)
}

/// The position directly before the innermost block quote a line sits in.
fn innermost_quote_before(types: &DocTypes, line: &Line) -> Option<usize> {
    line.ancestors()
        .iter()
        .rposition(|ancestor| Some(ancestor.node_type) == types.blockquote)
        .map(|index| line.ancestor_before(index))
}

/// Whether the line is the first one inside the quote it sits in: every
/// ancestor below the quote is that ancestor's first child.
fn opens_quote(types: &DocTypes, line: &Line) -> bool {
    let at = line
        .ancestors()
        .iter()
        .rposition(|ancestor| Some(ancestor.node_type) == types.blockquote);
    at.is_some_and(|at| {
        line.ancestors()[at + 1..]
            .iter()
            .all(|below| below.index == 0)
    })
}

fn attr<'a>(ancestor: &'a Ancestor, name: &str) -> &'a str {
    ancestor
        .attrs
        .get(name)
        .and_then(|value| value.as_str())
        .unwrap_or_default()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn every_callout_type_family_maps_to_its_own_tone() {
        for (kind, tone) in [
            ("note", Tone::Note),
            ("info", Tone::Note),
            ("todo", Tone::Note),
            ("abstract", Tone::Summary),
            ("summary", Tone::Summary),
            ("tldr", Tone::Summary),
            ("tip", Tone::Summary),
            ("hint", Tone::Summary),
            ("important", Tone::Summary),
            ("success", Tone::Success),
            ("check", Tone::Success),
            ("done", Tone::Success),
            ("question", Tone::Caution),
            ("help", Tone::Caution),
            ("faq", Tone::Caution),
            ("warning", Tone::Caution),
            ("caution", Tone::Caution),
            ("attention", Tone::Caution),
            ("failure", Tone::Danger),
            ("fail", Tone::Danger),
            ("missing", Tone::Danger),
            ("danger", Tone::Danger),
            ("error", Tone::Danger),
            ("bug", Tone::Danger),
            ("example", Tone::Example),
            ("quote", Tone::Quote),
            ("cite", Tone::Quote),
            // Case does not matter, and an unknown type is drawn in the
            // default style.
            ("WARNING", Tone::Caution),
            ("Tip", Tone::Summary),
            ("custom-type", Tone::Note),
            ("", Tone::Note),
        ] {
            assert_eq!(tone_of(kind), tone, "{kind:?}");
        }
    }

    #[test]
    fn every_tone_has_a_place_of_its_own() {
        let tones = [
            Tone::Note,
            Tone::Summary,
            Tone::Success,
            Tone::Caution,
            Tone::Danger,
            Tone::Example,
            Tone::Quote,
        ];
        let indexes: Vec<_> = tones.iter().map(|tone| tone.index()).collect();
        assert_eq!(indexes, (0..tones.len()).collect::<Vec<_>>());
    }

    #[test]
    fn a_header_reads_as_its_title_or_as_its_type_capitalised() {
        assert_eq!(
            head_of("note", ""),
            Some(Head {
                label: "Note".into(),
                tone: Tone::Note
            })
        );
        assert_eq!(
            head_of("tip", "Custom title").map(|h| h.label).as_deref(),
            Some("Custom title")
        );
        assert_eq!(
            head_of("NOTE", "").map(|h| h.label).as_deref(),
            Some("NOTE")
        );
        assert_eq!(
            head_of("custom-type", "").map(|h| h.label).as_deref(),
            Some("Custom-type")
        );
        // A title of nothing but spaces is no title.
        assert_eq!(
            head_of("note", "   ").map(|h| h.label).as_deref(),
            Some("Note")
        );
        // An ordinary quote has no header at all.
        assert_eq!(head_of("", "Title"), None);
    }

    #[test]
    fn localizing_callout_defaults_never_rewrites_authored_titles() {
        let messages = crate::EditorMessages::new(|message, args| match message {
            crate::EditorMessage::CalloutNote => "注意".into(),
            _ => crate::EditorMessages::ENGLISH.format(message, args),
        });
        assert_eq!(localized_head("note", "", &messages).unwrap().label, "注意");
        assert_eq!(
            localized_head("note", "Note", &messages).unwrap().label,
            "Note"
        );
        assert_eq!(
            localized_head("custom", "", &messages).unwrap().label,
            "Custom"
        );
    }
}
