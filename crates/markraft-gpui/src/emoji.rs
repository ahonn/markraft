//! Emoji by GitHub shortcode: a `:` menu and `:name:` auto-replace.
//!
//! Both read the [`emojis`] table and nothing else, so they carry no host resources and
//! a host only has to register them.

use crate::{
    EditorCx, Extension, Typeahead, TypeaheadItem, TypeaheadProvider, Update, typeahead::is_trigger,
};
use emojis::Emoji;
use markraft_core::{Block, BlockKind, Document, Mark, Origin, Position, Transaction};
use std::{any::Any, ops::Range, rc::Rc};
use unicode_segmentation::UnicodeSegmentation;

/// The extension id of the `:` menu; it names the edits it makes.
const EMOJI_MENU: &str = "emoji-menu";
/// The extension id of `:name:` auto-replace.
const EMOJI_SHORTCODES: &str = "emoji-shortcodes";

/// Under a Chinese input method `:` types the full-width `：`, so either opens the menu
/// and either closes a shortcode — including the mixed `:name：`, which costs nothing.
const COLONS: [char; 2] = [':', '：'];

/// How many rows the menu offers. A short query matches hundreds of shortcodes and only
/// the first screenful is ever looked at.
const LIMIT: usize = 30;

/// Two graphemes, so that `:D` followed by Return still inserts a newline and a lone `:`
/// never opens the menu.
const MIN_QUERY: usize = 2;

/// The `:` emoji menu.
pub fn emoji_menu() -> Typeahead {
    Typeahead::new(EMOJI_MENU, COLONS.to_vec(), EmojiProvider).min_query(MIN_QUERY)
}

struct EmojiProvider;

impl TypeaheadProvider for EmojiProvider {
    fn items(&self, query: &str) -> Vec<TypeaheadItem> {
        ranked(query)
            .into_iter()
            .map(|(emoji, shortcode)| {
                TypeaheadItem::new(shortcode, format!(":{shortcode}:")).glyph(emoji.as_str())
            })
            .collect()
    }

    fn accept(&self, item: &TypeaheadItem, tx: &mut Transaction<'_>) -> Option<Rc<dyn Any>> {
        // The trigger run is already gone, so this is the whole of the edit: the caret
        // lands after the emoji, on a grapheme boundary, with no trailing space.
        tx.insert_text(emojis::get_by_shortcode(&item.id)?.as_str());
        None
    }
}

/// Replaces `:name:` with its emoji as the closing colon is typed. It is its own
/// extension rather than part of the menu: it must also fire for a name the menu never
/// offered, such as the one-letter `:o:`, and the generic typeahead stays free of emoji
/// knowledge.
pub struct EmojiShortcodes;

impl Extension for EmojiShortcodes {
    fn id(&self) -> &'static str {
        EMOJI_SHORTCODES
    }

    /// Only the user's own typing arms the replacement. The edit it makes carries
    /// `Origin::Extension`, so the next round leaves it alone and the fixed point is
    /// reached; undoing it carries `Origin::History`, so the emoji does not come back.
    fn update(&mut self, update: &Update, cx: &mut EditorCx<'_>) {
        let typed = update
            .change
            .as_ref()
            .is_some_and(|change| change.origin == Origin::Typed);
        let selection = cx.selection();
        if !typed || cx.is_composing() || !selection.is_empty() {
            return;
        }
        let Some((run, emoji)) = closing_shortcode(cx.committed_document(), selection.head) else {
            return;
        };
        cx.transact(|tx| tx.replace_range(run, emoji));
    }
}

/// The `:shortcode:` whose closing colon `caret` sits after, and the emoji it stands
/// for. The opening colon must open the block or follow whitespace, so `10:30:` and
/// `a:b:` are text; the name must be a whole shortcode; and no part of the run may be
/// inline code or lie in a code block.
fn closing_shortcode(
    document: &Document,
    caret: Position,
) -> Option<(Range<Position>, &'static str)> {
    let caret = document.clamp_position(caret);
    let block = document.blocks.get(caret.block)?;
    if matches!(block.kind, BlockKind::Code { .. }) {
        return None;
    }
    let text = block.text();
    let before = text.get(..caret.byte)?;
    let (closing, close) = before.grapheme_indices(true).next_back()?;
    if !is_trigger(close, &COLONS) {
        return None;
    }
    // The same scan the menu's trigger uses: back to the start of the caret's word.
    let (opening, open) = before[..closing]
        .grapheme_indices(true)
        .rev()
        .take_while(|(_, grapheme)| !grapheme.chars().any(char::is_whitespace))
        .last()?;
    if !is_trigger(open, &COLONS) {
        return None;
    }
    let emoji = shortcode(&before[opening + open.len()..closing])?;
    let at = |byte| Position {
        block: caret.block,
        byte,
    };
    (!is_code(block, opening..caret.byte)).then(|| (at(opening)..at(caret.byte), emoji))
}

/// The emoji `name` spells exactly, case- and `-`/`_`-insensitively.
fn shortcode(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        return None;
    }
    if let Some(emoji) = emojis::get_by_shortcode(name) {
        return Some(emoji.as_str());
    }
    // Folding costs a scan of the table, which only a name that is not already written
    // the way gemoji writes it pays.
    let query: Vec<u8> = name.bytes().map(fold).collect();
    emojis::iter()
        .find(|emoji| {
            emoji
                .shortcodes()
                .any(|shortcode| rank(shortcode, &query) == Some(0))
        })
        .map(Emoji::as_str)
}

/// Whether any of `range` carries the code mark: an emoji must not replace text inside
/// an inline code span.
fn is_code(block: &Block, range: Range<usize>) -> bool {
    let mut end = 0;
    block.spans.iter().any(|span| {
        let start = end;
        end += span.text.len();
        span.marks.has(Mark::Code) && start < range.end && range.start < end
    })
}

/// The emoji whose shortcodes match `query`, best first: the whole shortcode, then its
/// start, then a match inside it. Ties keep the CLDR order [`emojis::iter`] yields, and
/// the list is capped at [`LIMIT`].
fn ranked(query: &str) -> Vec<(&'static Emoji, &'static str)> {
    let query: Vec<u8> = query.bytes().map(fold).collect();
    let mut matches: Vec<_> = emojis::iter()
        .filter_map(|emoji| {
            // An emoji with several shortcodes is offered once, under the one that
            // matched best, so `:satis` reads `:satisfied:` rather than `:laughing:`.
            let (rank, shortcode) = emoji
                .shortcodes()
                .filter_map(|shortcode| Some((rank(shortcode, &query)?, shortcode)))
                .min_by_key(|(rank, _)| *rank)?;
            Some((rank, emoji, shortcode))
        })
        .collect();
    matches.sort_by_key(|(rank, _, _)| *rank);
    matches.truncate(LIMIT);
    matches
        .into_iter()
        .map(|(_, emoji, shortcode)| (emoji, shortcode))
        .collect()
}

/// Shortcodes are lowercase ASCII in which both `-` and `_` occur — `t-rex` beside
/// `crossed_fingers` — so the two are folded together and a query written either way
/// finds either.
fn fold(byte: u8) -> u8 {
    if byte == b'-' {
        b'_'
    } else {
        byte.to_ascii_lowercase()
    }
}

/// 0 for the whole of `shortcode`, 1 for its start, 2 for the start of a later word in
/// it. A match inside a word does not count: `:-D` folds to `_d`, which sits inside
/// `hot_dog`, and a menu opening there would take the Return meant for a newline.
/// `query` is already folded.
fn rank(shortcode: &str, query: &[u8]) -> Option<u8> {
    if matches_at(shortcode, query, 0) {
        return Some(u8::from(shortcode.len() != query.len()));
    }
    let bytes = shortcode.as_bytes();
    (1..bytes.len())
        .any(|start| fold(bytes[start - 1]) == b'_' && matches_at(shortcode, query, start))
        .then_some(2)
}

fn matches_at(shortcode: &str, query: &[u8], start: usize) -> bool {
    let bytes = shortcode.as_bytes();
    start + query.len() <= bytes.len()
        && bytes[start..]
            .iter()
            .zip(query)
            .all(|(byte, wanted)| fold(*byte) == *wanted)
}

#[cfg(test)]
mod tests {
    use super::{EMOJI_SHORTCODES, MIN_QUERY, closing_shortcode, ranked};
    use crate::typeahead::open_match;
    use markraft_core::{
        Block, BlockKind, Document, Editor, Marks, Origin, Position, Selection, Span,
        TransactionOptions,
    };

    const COLONS: [char; 2] = [':', '：'];
    const CODE: Marks = Marks {
        bold: false,
        italic: false,
        code: true,
        strikethrough: false,
        underline: false,
    };

    fn labels(query: &str) -> Vec<String> {
        ranked(query)
            .into_iter()
            .map(|(_, shortcode)| shortcode.to_owned())
            .collect()
    }

    fn glyph(query: &str, shortcode: &str) -> String {
        ranked(query)
            .into_iter()
            .find(|(_, code)| *code == shortcode)
            .unwrap_or_else(|| panic!("{shortcode:?} among the matches for {query:?}"))
            .0
            .as_str()
            .to_owned()
    }

    fn doc(kind: BlockKind, spans: &[(&str, Marks)]) -> Document {
        let mut document = Document {
            blocks: vec![Block {
                kind,
                depth: 0,
                spans: spans
                    .iter()
                    .map(|(text, marks)| Span {
                        text: (*text).to_owned(),
                        marks: *marks,
                        link: None,
                    })
                    .collect(),
            }],
        };
        document.normalize();
        document
    }

    /// The replacement at the end of a plain paragraph holding `text`.
    fn found(text: &str) -> Option<String> {
        let document = doc(BlockKind::Paragraph, &[(text, Marks::default())]);
        closing_shortcode(
            &document,
            Position {
                block: 0,
                byte: text.len(),
            },
        )
        .map(|(_, emoji)| emoji.to_owned())
    }

    #[test]
    fn an_exact_shortcode_outranks_a_prefix_and_a_prefix_a_match_inside() {
        // "smiley" precedes "smile" in the table, and "grinning" precedes "grin", but
        // the whole shortcode wins either way.
        assert_eq!(labels("smile")[0], "smile");
        assert_eq!(labels("grin")[0], "grin");
        // A prefix comes before a match found inside a shortcode.
        assert_eq!(labels("potable"), ["potable_water", "non-potable_water"]);
    }

    #[test]
    fn a_match_inside_a_word_does_not_count() {
        // The start of a later word does; the middle of one does not.
        assert!(labels("water").contains(&"potable_water".to_owned()));
        assert!(!labels("dog").is_empty());
        assert!(!labels("og").contains(&"dog".to_owned()));
        // `:-D` folds to `_d`, which sits inside `hot_dog`.
        assert!(labels("-D").is_empty());
        assert!(labels("-)").is_empty());
    }

    #[test]
    fn the_list_is_capped_and_keeps_the_table_order_within_a_rank() {
        assert_eq!(labels("a").len(), super::LIMIT);
        // All prefixes of equal rank, so the table's own order decides.
        assert_eq!(&labels("smil")[..2], ["smiley", "smile"]);
    }

    #[test]
    fn case_underscores_and_hyphens_all_fold() {
        assert_eq!(labels("CROSSED_FING")[0], "crossed_fingers");
        assert_eq!(labels("crossed-fing")[0], "crossed_fingers");
        assert_eq!(labels("T_REX")[0], "t-rex");
        assert_eq!(labels("-1")[0], "-1");
        assert_eq!(labels("_1")[0], "-1");
    }

    #[test]
    fn the_glyph_is_the_emoji_itself() {
        assert_eq!(glyph("smile", "smile"), "😄");
        // A flag is two code points and a family several joined by ZWJ.
        assert_eq!(glyph("south_africa", "south_africa"), "🇿🇦");
        assert_eq!(
            glyph("family_woman_woman_girl", "family_woman_woman_girl"),
            "👩‍👩‍👧"
        );
    }

    /// Every emoji the menu can offer must be reachable by the closing colon too.
    #[test]
    fn the_menu_and_auto_replace_cover_the_same_table() {
        let with_shortcodes = emojis::iter()
            .filter(|emoji| emoji.shortcode().is_some())
            .count();
        assert!(
            with_shortcodes > 1500,
            "{with_shortcodes} emoji carry a shortcode"
        );
        for emoji in emojis::iter().filter(|emoji| emoji.shortcode().is_some()) {
            let shortcode = emoji.shortcode().unwrap();
            assert_eq!(
                found(&format!(":{shortcode}:")).as_deref(),
                Some(emoji.as_str()),
                "{shortcode}"
            );
        }
    }

    #[test]
    fn the_closing_colon_needs_a_word_opening_colon_and_a_whole_shortcode() {
        assert_eq!(found(":smile:").as_deref(), Some("😄"));
        assert_eq!(found("hello :smile:").as_deref(), Some("😄"));
        assert_eq!(found(":SMILE:").as_deref(), Some("😄"));
        // Mixed and full-width colons, which a Chinese input method types.
        assert_eq!(found("：smile：").as_deref(), Some("😄"));
        assert_eq!(found(":smile：").as_deref(), Some("😄"));
        // The opening colon must start the word.
        assert_eq!(found("10:30:"), None);
        assert_eq!(found("a:b:"), None);
        assert_eq!(found("smile:"), None);
        // And the name must be a whole shortcode, not a prefix or a stranger.
        assert_eq!(found(":smil:"), None);
        assert_eq!(found(":nosuchemoji:"), None);
        assert_eq!(found("::"), None);
        assert_eq!(found(":smile: "), None);
    }

    #[test]
    fn code_never_auto_replaces() {
        let code = doc(
            BlockKind::Code {
                language: String::new(),
            },
            &[(":smile:", Marks::default())],
        );
        assert!(
            closing_shortcode(
                &code,
                Position {
                    block: 0,
                    byte: ":smile:".len()
                }
            )
            .is_none()
        );
        // An inline code span, whether it covers the whole run or only its closing colon.
        let inline = doc(
            BlockKind::Paragraph,
            &[("x ", Marks::default()), (":smile:", CODE)],
        );
        assert!(
            closing_shortcode(
                &inline,
                Position {
                    block: 0,
                    byte: "x :smile:".len()
                }
            )
            .is_none()
        );
        let partial = doc(
            BlockKind::Paragraph,
            &[(":smile", Marks::default()), (":", CODE)],
        );
        assert!(
            closing_shortcode(
                &partial,
                Position {
                    block: 0,
                    byte: ":smile:".len()
                }
            )
            .is_none()
        );
        // Code that merely abuts the run does not stop it.
        let abutting = doc(
            BlockKind::Paragraph,
            &[("x", CODE), (" :smile:", Marks::default())],
        );
        assert_eq!(
            closing_shortcode(
                &abutting,
                Position {
                    block: 0,
                    byte: "x :smile:".len()
                }
            )
            .map(|(_, emoji)| emoji),
            Some("😄")
        );
    }

    /// The transaction [`EmojiShortcodes`] runs, driven directly on the core.
    fn replace(text: &str) -> Editor {
        let mut editor = Editor::new(Document::default());
        editor.insert_text_plain(text);
        let (run, emoji) = closing_shortcode(editor.document(), editor.selection().head)
            .expect("a shortcode at the caret");
        editor.transact(
            TransactionOptions {
                group: None,
                origin: Origin::Extension(EMOJI_SHORTCODES),
            },
            |tx| tx.replace_range(run, emoji),
        );
        editor
    }

    #[test]
    fn auto_replace_is_one_undo_step_that_restores_the_literal_text() {
        let mut editor = replace("hi :smile:");
        assert_eq!(editor.document().plain_text(), "hi 😄");
        assert_eq!(
            editor.selection(),
            Selection::caret(Position {
                block: 0,
                byte: "hi 😄".len()
            })
        );
        editor.undo();
        assert_eq!(editor.document().plain_text(), "hi :smile:");
        // The entry before it is the typing itself, so exactly one step was added.
        editor.undo();
        assert_eq!(editor.document().plain_text(), "");
    }

    #[test]
    fn undoing_carries_history_and_leaves_no_shortcode_to_replace_again() {
        let mut editor = replace(":smile:");
        let change = editor.undo().expect("an undo");
        assert_eq!(change.origin, Origin::History);
        // The text is `:smile:` again, so only the origin keeps the replacement from
        // firing on the undo's own change.
        assert!(closing_shortcode(editor.document(), editor.selection().head).is_some());
    }

    #[test]
    fn a_multi_codepoint_emoji_leaves_the_caret_on_a_grapheme_boundary() {
        for (text, emoji) in [
            (":south_africa:", "🇿🇦"),
            (":family_woman_woman_girl:", "👩‍👩‍👧"),
        ] {
            let editor = replace(text);
            assert_eq!(editor.document().plain_text(), emoji);
            let caret = editor.selection().head;
            assert_eq!(caret.byte, emoji.len());
            assert_eq!(editor.previous_position(caret).byte, 0);
        }
    }

    /// The replacement's own change must not arm it again.
    #[test]
    fn the_replacement_reaches_a_fixed_point() {
        let editor = replace(":smile:");
        assert!(closing_shortcode(editor.document(), editor.selection().head).is_none());
    }

    #[test]
    fn the_menu_stays_shut_until_the_query_is_long_enough() {
        let document = doc(BlockKind::Paragraph, &[(":D", Marks::default())]);
        let at = |byte, min| {
            open_match(&document, Position { block: 0, byte }, &COLONS, min)
                .map(|found| found.query)
        };
        assert_eq!(at(1, MIN_QUERY), None, "a lone colon opens nothing");
        assert_eq!(at(2, MIN_QUERY), None, "`:D` keeps Return to itself");
        assert_eq!(at(2, 0).as_deref(), Some("D"), "the `/` menu is unchanged");
        let document = doc(BlockKind::Paragraph, &[(":👩‍👩‍👧x", Marks::default())]);
        // Graphemes, not bytes: one family plus one letter is two.
        assert_eq!(
            open_match(
                &document,
                Position {
                    block: 0,
                    byte: ":👩‍👩‍👧".len()
                },
                &COLONS,
                MIN_QUERY,
            ),
            None
        );
        assert_eq!(
            open_match(
                &document,
                Position {
                    block: 0,
                    byte: ":👩‍👩‍👧x".len()
                },
                &COLONS,
                MIN_QUERY,
            )
            .map(|found| found.query)
            .as_deref(),
            Some("👩‍👩‍👧x")
        );
    }

    /// The two menus the app registers derive from the same caret, and their triggers
    /// are disjoint, so no caret ever opens both.
    #[test]
    fn the_slash_and_emoji_menus_are_never_open_together() {
        const SLASHES: [char; 2] = ['/', '、'];
        for text in [
            "/head", "、head", ":smile", "：smile", "x /a", "x :ab", "/:ab", ":/ab", "plain",
        ] {
            let document = doc(BlockKind::Paragraph, &[(text, Marks::default())]);
            let caret = Position {
                block: 0,
                byte: text.len(),
            };
            let slash = open_match(&document, caret, &SLASHES, 0).is_some();
            let emoji = open_match(&document, caret, &COLONS, MIN_QUERY).is_some();
            assert!(!(slash && emoji), "{text:?} opened both menus");
        }
    }

    #[test]
    fn a_marks_helper_covers_the_whole_run() {
        // Guards `is_code`'s span walk against an off-by-one at the run's edges.
        let block = &doc(
            BlockKind::Paragraph,
            &[
                ("ab", Marks::default()),
                ("cd", CODE),
                ("ef", Marks::default()),
            ],
        )
        .blocks[0];
        assert!(!super::is_code(block, 0..2));
        assert!(super::is_code(block, 1..3));
        assert!(super::is_code(block, 2..4));
        assert!(super::is_code(block, 3..6));
        assert!(!super::is_code(block, 4..6));
    }

    #[test]
    fn every_offered_item_can_be_accepted() {
        // The provider looks its emoji up again by the item's id.
        for (_, shortcode) in ranked("sm") {
            assert!(emojis::get_by_shortcode(shortcode).is_some(), "{shortcode}");
        }
        assert!(!ranked("sm").is_empty());
    }
}
