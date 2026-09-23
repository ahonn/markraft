//! Emoji by GitHub shortcode: a `:` menu and `:name:` auto-replace.
//!
//! Both read the [`emojis`] table and nothing else, so they carry no host resources and
//! a host only has to register them.

use crate::types::DocTypes;
use crate::{
    EditorCx, Extension, Typeahead, TypeaheadItem, TypeaheadProvider, Update, typeahead::is_trigger,
};
use emojis::Emoji;
use markraft_core::commands::{Command, command, insert_text};
use markraft_core::projection::Projection;
use markraft_core::{EditorState, Node};
use std::ops::Range;

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

    fn accept(&self, item: &TypeaheadItem) -> Command {
        // The trigger run is already gone, so this is the whole of the edit: the caret
        // lands after the emoji, on a grapheme boundary, with no trailing space.
        match emojis::get_by_shortcode(&item.id) {
            Some(emoji) => insert_text(emoji.as_str()),
            None => command(|_| None),
        }
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

    /// Only the user's own typing arms the replacement. The edit it makes carries this
    /// extension's origin, so the next round leaves it alone and the fixed point is
    /// reached; undoing it is an `undo` user event, so the emoji does not come back.
    fn update(&mut self, update: &Update, cx: &mut EditorCx<'_>) {
        let typed = update.is_user_event("input.type") && update.origin().is_none();
        if !typed || cx.is_composing() || !cx.selection().is_cursor() {
            return;
        }
        let spec = {
            let state = cx.state();
            let Some((run, emoji)) =
                closing_shortcode(state, &cx.projection(), cx.types(), cx.head())
            else {
                return;
            };
            let slice = markraft_core::Slice::from_fragment(markraft_core::Fragment::from_node(
                state.schema().text(emoji),
            ));
            markraft_core::commands::changes_spec(
                state,
                vec![
                    markraft_core::Change::replace(run.start, run.end, slice)
                        .with_fit(markraft_core::Fit::Auto),
                ],
                "input.replace",
            )
        };
        if let Some(spec) = spec {
            cx.dispatch([spec]);
        }
    }
}

/// The `:shortcode:` whose closing colon `caret` sits after, and the emoji it stands
/// for. The opening colon must open the line or follow whitespace, so `10:30:` and
/// `a:b:` are text; the name must be a whole shortcode; and no part of the run may be
/// inline code or lie in a verbatim block.
fn closing_shortcode(
    state: &EditorState,
    projection: &Projection,
    types: &DocTypes,
    caret: usize,
) -> Option<(Range<usize>, &'static str)> {
    if types.in_verbatim_block_at(state) {
        return None;
    }
    let index = projection.line_at(caret)?;
    let (closing, close) = projection.graphemes(index).rfind(|(pos, _)| *pos < caret)?;
    if !is_trigger(close, &COLONS) {
        return None;
    }
    // The same scan the menu's trigger uses: back to the start of the caret's word.
    let (opening, open) = projection
        .graphemes(index)
        .filter(|(pos, _)| *pos < closing)
        .rev()
        .take_while(|(_, grapheme)| !grapheme.chars().any(char::is_whitespace))
        .last()?;
    if !is_trigger(open, &COLONS) {
        return None;
    }
    let emoji = shortcode(projection.text_between(opening + open.chars().count(), closing)?)?;
    let range = opening..caret;
    (!is_code(state, types, range.start, range.end)).then_some((range, emoji))
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

/// Whether any of `from..to` carries the code mark: an emoji must not replace text
/// inside an inline code span.
fn is_code(state: &EditorState, types: &DocTypes, from: usize, to: usize) -> bool {
    let Some(code) = types.code else {
        return false;
    };
    let mut found = false;
    state
        .doc()
        .nodes_between(from, to, &mut |node: &Node, pos, _, _| {
            let end = pos + node.node_size();
            if node.marks().contains_type(code) && pos < to && from < end {
                found = true;
            }
            true
        });
    found
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
    use super::*;
    use crate::typeahead::open_match;
    use crate::typeahead::tests::{at, run, state_of, text_state};
    use markraft_core::projection::projection_of;

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

    /// The replacement at the end of a plain paragraph holding `source`.
    fn found_in(state: &EditorState, caret: usize) -> Option<(Range<usize>, &'static str)> {
        let types = crate::typeahead::tests::types_of(state);
        let state = at(state, caret);
        closing_shortcode(&state, &projection_of(&state), &types, caret)
    }

    fn found(text: &str) -> Option<String> {
        let state = text_state(text);
        let caret = 1 + text.chars().count();
        found_in(&state, caret).map(|(_, emoji)| emoji.to_owned())
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
        assert_eq!(labels("a").len(), LIMIT);
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
    fn verbatim_text_never_auto_replaces() {
        // A code block, and an inline code span covering the whole run or only its
        // closing colon.
        for source in ["```\n:smile:\n```", "x `:smile:`", "`:smile:`"] {
            let state = state_of(source);
            let caret = projection_of(&state)
                .lines()
                .last()
                .map(|line| line.to)
                .expect("a line");
            assert!(found_in(&state, caret).is_none(), "{source}");
        }
        // A raw block keeps its source as it was written, shortcode and all.
        let state = state_of("<div>\n:smile:\n</div>");
        let caret = projection_of(&state).lines()[0]
            .offset_to_pos("<div>\n:smile:".chars().count())
            .expect("an offset inside the block");
        assert!(found_in(&state, caret).is_none());
        // Code that merely abuts the run does not stop it.
        let state = state_of("`x` :smile:");
        let caret = projection_of(&state).lines()[0].to;
        assert_eq!(found_in(&state, caret).map(|(_, emoji)| emoji), Some("😄"));
    }

    /// The transaction [`EmojiShortcodes`] runs, driven directly on the state.
    fn replace(text: &str) -> EditorState {
        let state = run(&state_of(""), &insert_text(text));
        let types = crate::typeahead::tests::types_of(&state);
        let caret = state.selection().head(state.doc());
        let (run_range, emoji) = closing_shortcode(&state, &projection_of(&state), &types, caret)
            .expect("a shortcode at the caret");
        let slice = markraft_core::Slice::from_fragment(markraft_core::Fragment::from_node(
            state.schema().text(emoji),
        ));
        let spec = markraft_core::commands::changes_spec(
            &state,
            vec![
                markraft_core::Change::replace(run_range.start, run_range.end, slice)
                    .with_fit(markraft_core::Fit::Auto),
            ],
            "input.replace",
        )
        .expect("the replacement applies");
        state
            .update([spec])
            .expect("one transaction")
            .state()
            .clone()
    }

    #[test]
    fn auto_replace_is_one_undo_step_that_restores_the_literal_text() {
        let state = replace("hi :smile:");
        assert_eq!(projection_of(&state).plain_text(), "hi 😄");
        let state = run(&state, &command(markraft_core::history::undo));
        assert_eq!(projection_of(&state).plain_text(), "hi :smile:");
        // The entry before it is the typing itself, so exactly one step was added.
        let state = run(&state, &command(markraft_core::history::undo));
        assert_eq!(projection_of(&state).plain_text(), "");
    }

    #[test]
    fn a_multi_codepoint_emoji_leaves_the_caret_on_a_grapheme_boundary() {
        for (text, emoji) in [
            (":south_africa:", "🇿🇦"),
            (":family_woman_woman_girl:", "👩‍👩‍👧"),
        ] {
            let state = replace(text);
            let projection = projection_of(&state);
            assert_eq!(projection.plain_text(), emoji);
            let caret = state.selection().head(state.doc());
            assert_eq!(caret, 1 + emoji.chars().count());
            assert_eq!(projection.prev_grapheme_boundary(caret), Some(1));
        }
    }

    /// The literal text is back after an undo, so only the user event keeps the
    /// replacement from firing again on the undo's own change.
    #[test]
    fn undoing_is_not_typing_and_leaves_no_shortcode_to_replace_again() {
        let state = replace(":smile:");
        let spec = markraft_core::history::undo(&state).expect("an undo");
        let tr = state.update([spec]).expect("the undo applies");
        assert!(tr.is_user_event("undo"));
        assert!(!tr.is_user_event("input.type"));
        let state = tr.state().clone();
        let caret = state.selection().head(state.doc());
        assert!(found_in(&state, caret).is_some());
    }

    /// The replacement's own change must not arm it again.
    #[test]
    fn the_replacement_reaches_a_fixed_point() {
        let state = replace(":smile:");
        let caret = state.selection().head(state.doc());
        assert!(found_in(&state, caret).is_none());
    }

    #[test]
    fn the_menu_stays_shut_until_the_query_is_long_enough() {
        let state = state_of(":D");
        let projection = projection_of(&state);
        let at = |pos, min| open_match(&projection, false, pos, &COLONS, min).map(|f| f.query);
        assert_eq!(at(2, MIN_QUERY), None, "a lone colon opens nothing");
        assert_eq!(at(3, MIN_QUERY), None, "`:D` keeps Return to itself");
        assert_eq!(at(3, 0).as_deref(), Some("D"), "the `/` menu is unchanged");
        let family = "👩‍👩‍👧";
        let state = state_of(&format!(":{family}x"));
        let projection = projection_of(&state);
        let chars = family.chars().count();
        // Graphemes, not bytes: one family plus one letter is two.
        assert_eq!(
            open_match(&projection, false, 2 + chars, &COLONS, MIN_QUERY),
            None
        );
        assert_eq!(
            open_match(&projection, false, 3 + chars, &COLONS, MIN_QUERY)
                .map(|found| found.query)
                .as_deref(),
            Some(format!("{family}x").as_str())
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
            let state = state_of(text);
            let projection = projection_of(&state);
            let caret = projection.lines()[0].to;
            let slash = open_match(&projection, false, caret, &SLASHES, 0).is_some();
            let emoji = open_match(&projection, false, caret, &COLONS, MIN_QUERY).is_some();
            assert!(!(slash && emoji), "{text:?} opened both menus");
        }
    }

    #[test]
    fn the_code_mark_check_covers_the_whole_run() {
        // The backticks of `ab`cd`ef` are in the tree.
        // Positions: 1..2 ab, 3 `, 4..5 cd, 6 `, 7..8 ef
        let state = state_of("ab`cd`ef");
        let types = crate::typeahead::tests::types_of(&state);
        assert!(!is_code(&state, &types, 1, 3));
        assert!(is_code(&state, &types, 3, 4));
        assert!(is_code(&state, &types, 4, 6));
        assert!(is_code(&state, &types, 6, 7));
        assert!(!is_code(&state, &types, 7, 9));
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
