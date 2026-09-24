//! Property tests for the command catalogue, decoration mapping and the
//! projection, driven by the deterministic generator in `support`.

use crate::attr::Attrs;
use crate::change::{Change, ChangeSet};
use crate::commands::*;
use crate::decorations::{RangeItem, RangeSet};
use crate::fragment::Fragment;
use crate::history::{HistoryConfig, history, redo, undo};
use crate::node::Node;
use crate::projection::Projection;
use crate::schema::Schema;
use crate::selection::Selection;
use crate::slice::Slice;
use crate::state::{EditorState, Extension, TransactionSpec};

use super::support::*;

// Disambiguates from the change-building helper of the same name in `support`.
use crate::commands::insert_text;

/// Every command in the catalogue, paired with a name for failure messages.
fn catalogue(schema: &Schema) -> Vec<(&'static str, Command)> {
    let quote = schema.node_id("blockquote").expect("known");
    let heading = schema.node_id("heading").expect("known");
    let code = schema.node_id("code_block").expect("known");
    let list = schema.node_id("bullet_list").expect("known");
    let item = schema.node_id("list_item").expect("known");
    let br = schema.node_id("hard_break").expect("known");
    let strong = schema.mark_id("strong").expect("known");
    let link = schema.mark_id("link").expect("known");
    vec![
        ("delete_selection", delete_selection()),
        ("join_backward", join_backward()),
        ("join_forward", join_forward()),
        ("select_node_backward", select_node_backward()),
        ("select_node_forward", select_node_forward()),
        ("join_textblock_backward", join_textblock_backward()),
        ("join_textblock_forward", join_textblock_forward()),
        ("join_up", join_up()),
        ("join_down", join_down()),
        ("split_block", split_block()),
        ("split_block_keep_marks", split_block_keep_marks()),
        ("lift_empty_block", lift_empty_block()),
        ("new_line_in_code", new_line_in_code()),
        ("exit_code", exit_code()),
        ("create_paragraph_near", create_paragraph_near()),
        ("lift", lift()),
        ("wrap_in", wrap_in(quote, Attrs::empty())),
        (
            "set_block_type.heading",
            set_block_type(heading, crate::attrs! {"level" => 2i64}),
        ),
        ("set_block_type.code", set_block_type(code, Attrs::empty())),
        ("select_all", select_all()),
        ("select_parent_node", select_parent_node()),
        ("select_textblock_start", select_textblock_start()),
        ("select_textblock_end", select_textblock_end()),
        ("toggle_mark.strong", toggle_mark(strong, Attrs::empty())),
        (
            "toggle_mark.link",
            toggle_mark(link, crate::attrs! {"href" => "https://example.com"}),
        ),
        ("insert_text", insert_text("Z")),
        ("insert_hard_break", insert_hard_break(br)),
        ("wrap_in_list", wrap_in_list(list, Attrs::empty())),
        ("split_list_item", split_list_item(item)),
        ("lift_list_item", lift_list_item(item)),
        ("sink_list_item", sink_list_item(item)),
        (
            "move_by_grapheme.forward",
            move_by_grapheme(Direction::Forward, false),
        ),
        (
            "move_by_grapheme.backward",
            move_by_grapheme(Direction::Backward, true),
        ),
        (
            "move_by_word.forward",
            move_by_word(Direction::Forward, false),
        ),
        (
            "move_by_word.backward",
            move_by_word(Direction::Backward, true),
        ),
        (
            "delete_by_grapheme.backward",
            delete_by_grapheme(Direction::Backward),
        ),
        (
            "delete_by_grapheme.forward",
            delete_by_grapheme(Direction::Forward),
        ),
        (
            "delete_by_word.backward",
            delete_by_word(Direction::Backward),
        ),
        ("delete_by_word.forward", delete_by_word(Direction::Forward)),
    ]
}

/// A handful of selections to try on one document.
fn selections(schema: &Schema, rng: &mut Rng, document: &Node) -> Vec<Selection> {
    let spots = textblock_positions(schema, document);
    let mut out = vec![Selection::All];
    if spots.is_empty() {
        return out;
    }
    for _ in 0..4 {
        let (_, pos) = *rng.pick(&spots);
        out.push(Selection::cursor(pos));
    }
    let (_, a) = *rng.pick(&spots);
    let (_, b) = *rng.pick(&spots);
    out.push(Selection::text(a, b));
    for (before, _) in block_spans(schema, document) {
        if Selection::is_selectable(schema, document, before) {
            out.push(Selection::node(before));
            break;
        }
    }
    out
}

#[test]
fn every_command_leaves_a_valid_document_and_selection() {
    let schema = shared_schema();
    let commands = catalogue(&schema);
    let mut rng = Rng::new(0x5eed_1234);
    for _ in 0..40 {
        let document = random_doc(&schema, &mut rng);
        for selection in selections(&schema, &mut rng, &document) {
            let Ok(start) = EditorState::create(
                crate::state::EditorStateConfig::new(schema.clone())
                    .doc(document.clone())
                    .selection(selection.clone())
                    .extensions(Extension::none()),
            ) else {
                continue;
            };
            for (name, command) in &commands {
                let Some(result) = run_command(&start, command) else {
                    continue;
                };
                let tr = result.unwrap_or_else(|error| {
                    panic!("`{name}` produced an unusable transaction: {error}")
                });
                let next = tr.state();
                next.doc().check(&schema).unwrap_or_else(|error| {
                    panic!(
                        "`{name}` produced an invalid document: {error}\n{}",
                        schema.describe(next.doc())
                    )
                });
                next.selection()
                    .check(next.doc(), &schema)
                    .unwrap_or_else(|error| {
                        panic!("`{name}` produced an invalid selection: {error}")
                    });
            }
        }
    }
}

/// Every command that changes the document is one undo step: undo gives back the
/// document and selection it started from, and redo gives back what it made.
#[test]
fn every_command_undoes_to_where_it_started_and_redoes_to_what_it_made() {
    let schema = shared_schema();
    let commands = catalogue(&schema);
    let mut rng = Rng::new(0x5eed_4321);
    let mut checked = 0;
    for _ in 0..40 {
        let document = random_doc(&schema, &mut rng);
        for selection in selections(&schema, &mut rng, &document) {
            let Ok(start) = EditorState::create(
                crate::state::EditorStateConfig::new(schema.clone())
                    .doc(document.clone())
                    .selection(selection.clone())
                    .extensions(history(HistoryConfig::default())),
            ) else {
                continue;
            };
            for (name, command) in &commands {
                let Some(Ok(tr)) = run_command(&start, command) else {
                    continue;
                };
                let next = tr.state().clone();
                if next.doc() == start.doc() {
                    continue;
                }
                checked += 1;
                let undone = next
                    .update(
                        [undo(&next).unwrap_or_else(|| panic!("`{name}` left nothing to undo"))],
                    )
                    .unwrap_or_else(|error| panic!("undoing `{name}` failed: {error}"))
                    .state()
                    .clone();
                assert_eq!(
                    undone.doc(),
                    start.doc(),
                    "undoing `{name}` gave another document\nwas:  {}\nundone: {}",
                    schema.describe(start.doc()),
                    schema.describe(undone.doc())
                );
                assert_eq!(
                    undone.selection(),
                    start.selection(),
                    "undoing `{name}` moved the selection in {}",
                    schema.describe(start.doc())
                );
                let redone = undone
                    .update([
                        redo(&undone).unwrap_or_else(|| panic!("`{name}` left nothing to redo"))
                    ])
                    .unwrap_or_else(|error| panic!("redoing `{name}` failed: {error}"))
                    .state()
                    .clone();
                assert_eq!(
                    redone.doc(),
                    next.doc(),
                    "redoing `{name}` gave another document\nmade:   {}\nredone: {}",
                    schema.describe(next.doc()),
                    schema.describe(redone.doc())
                );
            }
        }
    }
    assert!(checked > 100, "only {checked} edits were checked");
}

#[test]
fn delete_range_over_random_spans_stays_valid() {
    let schema = shared_schema();
    let mut rng = Rng::new(0xdeed_0001);
    for _ in 0..40 {
        let document = random_doc(&schema, &mut rng);
        let spots = textblock_positions(&schema, &document);
        if spots.is_empty() {
            continue;
        }
        let (_, a) = *rng.pick(&spots);
        let (_, b) = *rng.pick(&spots);
        let (from, to) = (a.min(b), a.max(b));
        let start = state(document.clone(), Extension::none());
        let command = delete_range(from, to);
        let Some(result) = run_command(&start, &command) else {
            continue;
        };
        let tr = result.expect("delete_range resolves");
        tr.state().doc().check(&schema).expect("valid document");
    }
}

/// A change set that deletes `from..to` from `doc`.
fn deletion(schema: &Schema, doc: &Node, from: usize, to: usize) -> Option<ChangeSet> {
    ChangeSet::create(schema, doc, [Change::delete(from, to)]).ok()
}

#[test]
fn range_sets_stay_inside_the_mapped_document() {
    let schema = shared_schema();
    let mut rng = Rng::new(0xfeed_0002);
    for _ in 0..60 {
        let document = random_doc(&schema, &mut rng);
        let spots = textblock_positions(&schema, &document);
        if spots.len() < 2 {
            continue;
        }
        let (start, a) = *rng.pick(&spots);
        let (start_b, b) = *rng.pick(&spots);
        if start != start_b || a == b {
            continue;
        }
        let (from, to) = (a.min(b), a.max(b));
        let Some(changes) = deletion(&schema, &document, from, to) else {
            continue;
        };
        let desc = changes.desc();
        let items = vec![
            RangeItem::new(from, to, "inside"),
            RangeItem::new(start, document.content_size(), "outside"),
            RangeItem::new(from, from, "empty"),
        ];
        let set = RangeSet::from_items(items);
        let mapped = set.map(desc);
        let kept: Vec<&str> = mapped.iter().map(|item| item.value).collect();
        assert!(
            !kept.contains(&"inside"),
            "a range whose content is deleted must be dropped"
        );
        assert!(kept.contains(&"empty"), "an empty range marks a position");
        for item in mapped.iter() {
            assert!(item.from <= item.to);
            assert!(item.to <= desc.length_after());
        }
    }
}

#[test]
fn a_random_insertion_never_moves_a_range_out_of_the_document() {
    let schema = shared_schema();
    let mut rng = Rng::new(0xfeed_0003);
    for _ in 0..60 {
        let document = random_doc(&schema, &mut rng);
        let spots = textblock_positions(&schema, &document);
        if spots.is_empty() {
            continue;
        }
        let (_, at) = *rng.pick(&spots);
        let Ok(changes) = ChangeSet::create(
            &schema,
            &document,
            [Change::insert(
                at,
                Slice::from_fragment(Fragment::from_node(schema.text("QQ"))),
            )],
        ) else {
            continue;
        };
        let desc = changes.desc();
        let set = RangeSet::from_items(
            (0..=document.content_size())
                .step_by(3)
                .map(|pos| RangeItem::new(pos, (pos + 2).min(document.content_size()), "r")),
        );
        for item in set.map(desc).iter() {
            assert!(item.from <= item.to);
            assert!(item.to <= desc.length_after());
        }
    }
}

#[test]
fn projection_round_trips_for_random_documents() {
    let schema = shared_schema();
    let mut rng = Rng::new(0xfeed_0004);
    for _ in 0..60 {
        let document = random_doc(&schema, &mut rng);
        let projection = Projection::of(&document, &schema);

        let joined: Vec<&str> = (0..projection.line_count())
            .map(|index| projection.line_text(index).expect("line text"))
            .collect();
        assert_eq!(projection.plain_text(), joined.join("\n"));

        for pos in 0..=document.content_size() {
            let Some((line, offset)) = projection.pos_to_line_offset(pos) else {
                continue;
            };
            assert_eq!(projection.line_offset_to_pos(line, offset), Some(pos));
            let units = projection.pos_to_utf16(pos).expect("utf16 offset");
            assert_eq!(projection.utf16_to_pos(units), Some(pos));
            assert_eq!(
                projection.utf16_offset(line, pos),
                projection
                    .pos_to_utf16(pos)
                    .map(|u| u - projection.line(line).expect("line").utf16_start())
            );

            // Boundary helpers stay inside the document and on real boundaries.
            if let Some(next) = projection.next_grapheme_boundary(pos) {
                assert!(next <= document.content_size());
                assert!(projection.is_grapheme_boundary(next));
            }
            if let Some(previous) = projection.prev_grapheme_boundary(pos) {
                assert!(projection.is_grapheme_boundary(previous));
            }
            if let Some(word) = projection.next_word_boundary(pos) {
                assert!(word <= document.content_size());
            }
        }
    }
}

#[test]
fn commands_keep_the_caret_on_a_grapheme_boundary() {
    let schema = shared_schema();
    let mut rng = Rng::new(0xfeed_0005);
    let motions = [
        move_by_grapheme(Direction::Forward, false),
        move_by_grapheme(Direction::Backward, false),
        move_by_word(Direction::Forward, false),
        move_by_word(Direction::Backward, false),
    ];
    for _ in 0..30 {
        let document = random_doc(&schema, &mut rng);
        let spots = textblock_positions(&schema, &document);
        if spots.is_empty() {
            continue;
        }
        let (_, pos) = *rng.pick(&spots);
        let start = state(document.clone(), crate::projection::projection());
        let Ok(start) = start.update([TransactionSpec::new().selection(Selection::cursor(pos))])
        else {
            continue;
        };
        let start = start.state().clone();
        for command in &motions {
            let Some(result) = run_command(&start, command) else {
                continue;
            };
            let next = result.expect("motion resolves").state().clone();
            let projection = crate::projection::projection_of(&next);
            let head = next.selection().head(next.doc());
            assert!(
                projection.is_grapheme_boundary(head) || projection.line_at(head).is_none(),
                "the caret must not land inside a grapheme cluster"
            );
        }
    }
}
