//! The key chains of `markraft_core::kind::chains`, over the CommonMark kind:
//! what Enter, Backspace, Delete, Tab and the block toggles do where.

use markraft_commonmark::{
    commonmark_doc_type_names, commonmark_extensions, commonmark_schema, from_markdown,
    schema as md, to_markdown,
};
use markraft_core::commands::{Command, Direction, delete_range, run_command};
use markraft_core::kind::chains::*;
use markraft_core::kind::{DocTypes, DocumentKind, PlainKind};
use markraft_core::projection::projection_of;
use markraft_core::{Attrs, EditorState, EditorStateConfig, Extension, Selection, TransactionSpec};

/// A state on the CommonMark schema with the editor's own extensions.
fn state_of(source: &str) -> EditorState {
    let schema = commonmark_schema();
    let doc = from_markdown(&schema, source).expect("valid Markdown");
    EditorState::create(
        EditorStateConfig::new(schema.clone())
            .doc(doc)
            .extensions(Extension::all([
                markraft_core::projection::projection(),
                markraft_core::composition::composition(),
                markraft_core::history::history(Default::default()),
                commonmark_extensions(&schema),
            ])),
    )
    .expect("a valid state")
}

/// The CommonMark roles, as a host wires them.
fn types_of(state: &EditorState) -> DocTypes {
    DocTypes::from_schema_names(state.schema(), &commonmark_doc_type_names())
}

/// Move the caret to `pos`.
fn at(state: &EditorState, pos: usize) -> EditorState {
    state
        .update([TransactionSpec::new().selection(Selection::cursor(pos))])
        .expect("a selection")
        .state()
        .clone()
}

/// Run `command`, which has to apply.
fn run(state: &EditorState, command: &Command) -> EditorState {
    run_command(state, command)
        .expect("the command applies")
        .expect("a transaction")
        .state()
        .clone()
}

/// Run `command` and give back the Markdown it leaves, or `None` when the
/// command does not apply.
fn after(state: &EditorState, command: &Command) -> Option<String> {
    let tr = run_command(state, command)?.expect("a transaction");
    Some(to_markdown(state.schema(), tr.state().doc()))
}

/// The state a command leaves, or `None` when it does not apply.
fn applied(state: &EditorState, command: &Command) -> Option<EditorState> {
    Some(
        run_command(state, command)?
            .expect("a transaction")
            .state()
            .clone(),
    )
}

/// The row and column the caret sits at, or `None` outside a table.
fn cell_of(state: &EditorState) -> Option<(usize, usize)> {
    let types = types_of(state).table_types()?;
    markraft_core::commands::cell_at(types, state).map(|at| (at.row, at.column))
}

/// A two-by-two table, and the Markdown it reads back as.
fn table_state() -> (EditorState, String) {
    let state = state_of("| a | b |\n| - | - |\n| c | d |");
    let markdown = to_markdown(state.schema(), state.doc());
    (state, markdown)
}

/// The caret at the start of the line holding `needle`.
fn caret_in(state: &EditorState, needle: &str) -> usize {
    let projection = projection_of(state);
    let line = projection
        .lines()
        .iter()
        .find(|line| {
            projection
                .line_text(projection.line_at(line.from()).expect("a line"))
                .is_some_and(|text| text == needle)
        })
        .expect("a line holding the text");
    line.from()
}

#[test]
fn enter_splits_a_list_item_and_leaves_the_list_from_an_empty_one() {
    let state = state_of("- one");
    let state = at(&state, caret_in(&state, "one") + 3);
    let split = after(&state, &enter(&types_of(&state))).expect("the split applies");
    assert_eq!(split, "- one\n- ");
    // Enter again, in the now empty item, leaves the list: the empty block
    // becomes a paragraph. CommonMark has no empty-paragraph spelling, so
    // the file just ends after the list's blank separator.
    let state = state_of("- one\n-\n");
    let state = at(&state, projection_of(&state).lines()[1].to());
    let lifted = after(&state, &enter(&types_of(&state))).expect("the lift applies");
    assert_eq!(lifted, "- one");
}

/// With the document kind's split wrapper, Enter inside a style keeps it on
/// both sides — in a list item as in a paragraph.
#[test]
fn enter_with_a_kinds_split_keeps_the_style_open_at_the_caret() {
    let formatter = markraft_commonmark::Formatter::new(Default::default());
    /// A kind that keeps styles open across a split, and nothing else.
    struct KeepingStyles(markraft_commonmark::Formatter);
    impl DocumentKind for KeepingStyles {
        fn wrap_split(
            &self,
            split: markraft_core::commands::Command,
        ) -> markraft_core::commands::Command {
            self.0.keeping_styles(split)
        }
    }
    let kind = KeepingStyles(formatter);
    for (source, line, expected) in [
        ("**abcd**", "**abcd**", "**ab**\n\n**cd**"),
        ("- **abcd**", "**abcd**", "- **ab**\n- **cd**"),
    ] {
        let state = state_of(source);
        let state = at(&state, caret_in(&state, line) + 4);
        let enter = enter_with(&types_of(&state), &kind);
        assert_eq!(after(&state, &enter).as_deref(), Some(expected), "{source}");
    }
}

/// Backspace at the start of a nested list's first item joins it to the
/// item the list is in, as a paragraph of that item, as Typora does; in a
/// top-level list's first item it leaves the list altogether.
#[test]
fn backspace_at_a_first_items_start_joins_the_item_above_or_leaves_the_list() {
    let state = state_of("- one\n  - two\n  - three");
    let state = at(&state, caret_in(&state, "two"));
    let joined = after(&state, &backspace(&types_of(&state))).expect("the join applies");
    assert_eq!(joined, "- one\n\n  two\n\n  - three");
    let state = state_of("- one\n- two");
    let state = at(&state, caret_in(&state, "one"));
    let lifted = after(&state, &backspace(&types_of(&state))).expect("the lift applies");
    assert_eq!(lifted, "one\n\n- two");
}

#[test]
fn backspace_at_a_later_items_start_joins_it_to_the_item_before() {
    for (source, line, expected) in [
        ("- one\n- two", "two", "- one\n\n  two"),
        ("- one\n- two\n- three", "two", "- one\n\n  two\n\n- three"),
        ("1. one\n2. two", "two", "1. one\n\n   two"),
        ("- one\n  - a\n  - b", "b", "- one\n  - a\n\n    b"),
    ] {
        let state = state_of(source);
        let state = at(&state, caret_in(&state, line));
        let joined = after(&state, &backspace(&types_of(&state))).expect("the join applies");
        assert_eq!(joined, expected, "{source:?}");
    }
}

#[test]
fn backspace_inside_a_block_deletes_one_grapheme_and_joins_at_its_start() {
    let state = state_of("ab");
    let state = at(&state, 3);
    assert_eq!(
        after(&state, &backspace(&types_of(&state))).as_deref(),
        Some("a")
    );
    // A grapheme cluster goes as a whole.
    let state = state_of("a👩‍👩‍👧");
    let end = projection_of(&state).lines()[0].to();
    let state = at(&state, end);
    assert_eq!(
        after(&state, &backspace(&types_of(&state))).as_deref(),
        Some("a")
    );
    // At a paragraph's start the two blocks join instead.
    let state = state_of("one\n\ntwo");
    let state = at(&state, caret_in(&state, "two"));
    assert_eq!(
        after(&state, &backspace(&types_of(&state))).as_deref(),
        Some("onetwo")
    );
}

#[test]
fn tab_and_shift_tab_sink_and_lift_a_list_item() {
    let state = state_of("- one\n- two");
    let state = at(&state, caret_in(&state, "two"));
    let sunk = after(&state, &indent(&types_of(&state), "\t")).expect("the sink applies");
    assert_eq!(sunk, "- one\n  - two");
    let state = state_of("- one\n  - two");
    let state = at(&state, caret_in(&state, "two"));
    let lifted = after(&state, &outdent(&types_of(&state), "\t")).expect("the lift applies");
    assert_eq!(lifted, "- one\n- two");
}

/// Over a selection in a code block, Tab indents every line it covers and
/// Shift-Tab outdents them; a line the selection only reaches the start of
/// stays as it is. Shift-Tab at a caret outdents its own line.
#[test]
fn tab_and_shift_tab_shift_the_lines_of_a_code_selection() {
    let state = state_of("```\na\n\tb\n    c\nd\n```");
    let types = types_of(&state);
    let first = projection_of(&state).lines()[0].from();
    let select = |from: usize, to: usize| {
        state
            .update([TransactionSpec::new().selection(Selection::text(first + from, first + to))])
            .unwrap()
            .state()
            .clone()
    };
    // `a` to the start of `d`: three lines.
    let indented = after(&select(0, 11), &indent(&types, "\t"));
    assert_eq!(
        indented.as_deref(),
        Some("```\n\ta\n\t\tb\n\t    c\nd\n```")
    );
    let outdented = after(&select(0, 11), &outdent(&types, "\t"));
    assert_eq!(outdented.as_deref(), Some("```\na\nb\nc\nd\n```"));
    let spaces = after(&select(0, 11), &indent(&types, "  "));
    assert_eq!(spaces.as_deref(), Some("```\n  a\n  \tb\n      c\nd\n```"));
    let caret = after(&at(&state, first + 3), &outdent(&types, "\t"));
    assert_eq!(caret.as_deref(), Some("```\na\nb\n    c\nd\n```"));
}

#[test]
fn tab_in_a_code_block_inserts_the_indent_text_it_is_given() {
    let state = state_of("```\nab\n```");
    let state = at(&state, caret_in(&state, "ab") + 1);
    let types = types_of(&state);
    assert_eq!(
        after(&state, &indent(&types, "\t")).as_deref(),
        Some("```\na\tb\n```")
    );
    assert_eq!(
        after(&state, &indent(&types, "    ")).as_deref(),
        Some("```\na    b\n```")
    );
}

/// Return in a code block starts the new line at the depth of the line it
/// splits, in the spaces or tabs that line uses, and no deeper than the
/// caret: splitting inside the indent carries only what lies before it.
#[test]
fn return_in_a_code_block_keeps_the_lines_indent() {
    for (source, needle, offset, typed) in [
        (
            "```\nfn a() {\n    x\n```",
            "    x",
            5,
            "```\nfn a() {\n    x\n    y\n```",
        ),
        ("```\n\tx\n```", "\tx", 2, "```\n\tx\n\ty\n```"),
        ("```\n  \t x\n```", "  \t x", 5, "```\n  \t x\n  \t y\n```"),
        ("```\n    x\n```", "    x", 2, "```\n  \n  yx\n```"),
        ("```\nx\n```", "x", 1, "```\nx\ny\n```"),
    ] {
        let state = state_of(source);
        let mut start = None;
        state.doc().descendants(&mut |node, pos, _, _| {
            if let Some(at) = node.text().and_then(|text| text.find(needle)) {
                start = start.or(Some(pos + at));
            }
            true
        });
        let state = at(&state, start.expect("the line") + offset);
        let entered = applied(&state, &enter(&types_of(&state))).expect("a new line");
        let entered = run(&entered, &markraft_core::commands::insert_text("y"));
        assert_eq!(
            to_markdown(state.schema(), entered.doc()),
            typed,
            "{source:?}"
        );
    }
}

/// Return at the end of a paragraph that more blocks of its item follow
/// splits the item, as Typora does: the new item takes those blocks. Until
/// something is typed it opens on an empty line, which the source holds as
/// the marker alone on its line.
#[test]
fn return_before_more_blocks_of_an_item_splits_it() {
    for (source, new) in [
        ("- first\n\n  para2\n- next\n", "- new\n\n  para2"),
        (
            "- [ ] first\n\n  para2\n- [ ] next\n",
            "- [ ] new\n\n  para2",
        ),
        ("- first\n  - sub\n- next\n", "- new\n  - sub"),
    ] {
        let state = state_of(source);
        let state = at(&state, caret_in(&state, "first") + 5);
        let entered = applied(&state, &enter(&types_of(&state))).expect("a split");
        let baseline = markraft_commonmark::SourceDocument::parse(state.schema(), source)
            .expect("the source parses");
        assert!(
            baseline.render(state.schema(), entered.doc()).is_ok(),
            "{source:?}: {}",
            state.schema().describe(entered.doc())
        );
        let typed = run(&entered, &markraft_core::commands::insert_text("new"));
        let markdown = to_markdown(state.schema(), typed.doc());
        assert!(markdown.contains(new), "{source:?}: {markdown:?}");
        assert!(
            baseline.render(state.schema(), typed.doc()).is_ok(),
            "{source:?}: {markdown:?}"
        );
    }
    // A code block keeps Return for its own newlines, wherever it is in
    // the item: no split cuts it in two or moves what follows it.
    for (source, offset, expected) in [
        (
            "- ```\n  ab\n  ```\n\n  para\n",
            2,
            "- ```\n  ab\n  \n  ```\n\n  para",
        ),
        ("- ```\n  ab\n  ```\n", 1, "- ```\n  a\n  b\n  ```"),
        (
            "- [ ] \n  ```\n  ab\n  ```\n",
            2,
            "- [ ] \n  ```\n  ab\n  \n  ```",
        ),
    ] {
        let state = state_of(source);
        let state = at(&state, caret_in(&state, "ab") + offset);
        assert_eq!(
            after(&state, &enter(&types_of(&state))).as_deref(),
            Some(expected),
            "{source:?}"
        );
    }
    // Where nothing follows in the item, Return still splits it.
    let state = state_of("- first\n\n  para2\n- next\n");
    let state = at(&state, caret_in(&state, "para2") + 5);
    assert_eq!(
        after(&state, &enter(&types_of(&state))).as_deref(),
        Some("- first\n\n  para2\n\n- \n\n- next")
    );
}

/// A list shortcut in a list of another kind turns the whole list into the
/// kind asked for, as Typora does, rather than nesting a new list in the
/// item. The new list is marked with the attributes the shortcut gives.
#[test]
fn a_list_shortcut_converts_the_whole_list_it_is_in() {
    let state = state_of("1) a\n2) b\n");
    let state = at(&state, caret_in(&state, "b"));
    let types = types_of(&state);
    let (bullet, ordered) = (types.bullet_list.unwrap(), types.ordered_list.unwrap());
    let (item, task) = (types.list_item.unwrap(), types.task_item.unwrap());
    let stars = toggle_list(
        &types,
        bullet,
        markraft_core::attrs! {"bullet_char" => "*"},
        item,
    );
    assert_eq!(after(&state, &stars).as_deref(), Some("* a\n* b"));
    let tasks = toggle_list(&types, bullet, Attrs::empty(), task);
    assert_eq!(after(&state, &tasks).as_deref(), Some("- [ ] a\n- [ ] b"));

    let state = state_of("* a\n* b\n  - nested\n");
    let state = at(&state, caret_in(&state, "b"));
    let numbers = toggle_list(&types, ordered, Attrs::empty(), item);
    assert_eq!(
        after(&state, &numbers).as_deref(),
        Some("1. a\n2. b\n   - nested"),
        "a nested list keeps its kind"
    );
    let tasks = toggle_list(&types, bullet, Attrs::empty(), task);
    assert_eq!(
        after(&state, &tasks).as_deref(),
        Some("* [ ] a\n* [ ] b\n  - nested"),
        "the same list keeps its marker when only its items change"
    );

    let state = state_of("- [x] a\n- [ ] b\n");
    let state = at(&state, caret_in(&state, "b"));
    let numbers = toggle_list(&types, ordered, Attrs::empty(), task);
    assert_eq!(
        after(&state, &numbers).as_deref(),
        Some("1. [x] a\n2. [ ] b"),
        "a task keeps its box"
    );
    let loose = state_of("1. a\n\n2. b\n");
    let loose = at(&loose, caret_in(&loose, "b"));
    assert_eq!(
        after(&loose, &toggle_list(&types, bullet, Attrs::empty(), item)).as_deref(),
        Some("- a\n\n- b"),
        "the spacing between items stays"
    );
}

/// Tab walks a table in row-major order and grows it rather than falling
/// out of it, which is what it does in Typora and Bear.
#[test]
fn tab_walks_the_cells_and_appends_a_row_past_the_last_one() {
    let (state, markdown) = table_state();
    let types = types_of(&state);
    let lines = projection_of(&state);
    let (first, last) = (lines.lines()[0].from(), lines.lines()[3].to());
    let stepped = applied(&at(&state, first), &indent(&types, "\t")).expect("Tab steps right");
    assert_eq!(to_markdown(state.schema(), stepped.doc()), markdown);
    assert_eq!(cell_of(&stepped), Some((0, 1)));
    let grown = applied(&at(&state, last), &indent(&types, "\t")).expect("Tab grows the table");
    assert_eq!(projection_of(&grown).lines().len(), 6, "a row was appended");
    assert_eq!(cell_of(&grown), Some((2, 0)));
    // ⇧Tab steps back, and stops rather than lifting the first cell out of
    // its row, which would leave that row one cell short.
    let back = applied(&at(&state, lines.lines()[1].from()), &outdent(&types, "\t"))
        .expect("⇧Tab steps left");
    assert_eq!(cell_of(&back), Some((0, 0)));
    let stopped = applied(&at(&state, first), &outdent(&types, "\t"));
    assert!(stopped.is_none(), "⇧Tab in the first cell does nothing");
}

/// A cell that split would leave its row one cell wider than the rest, so
/// Enter moves down a row instead, appending one at the bottom.
#[test]
fn enter_moves_down_a_row_and_never_splits_a_cell() {
    let (state, markdown) = table_state();
    let types = types_of(&state);
    let lines = projection_of(&state);
    let inside = lines.lines()[0].to();
    let moved = applied(&at(&state, inside), &enter(&types)).expect("Enter applies");
    assert_eq!(
        to_markdown(state.schema(), moved.doc()),
        markdown,
        "nothing was split"
    );
    assert_eq!(cell_of(&moved), Some((1, 0)));
    let grown = applied(&at(&state, lines.lines()[3].to()), &enter(&types)).expect("Enter");
    assert_eq!(projection_of(&grown).lines().len(), 6, "a row was appended");
    assert_eq!(cell_of(&grown), Some((2, 1)));
    // ⌘⏎ adds a row under the caret's own row rather than at the bottom,
    // and moves into its first cell to fill it in, as Typora does.
    let added = applied(&at(&state, inside), &toggle_task(&types)).expect("⌘⏎ adds a row");
    assert_eq!(projection_of(&added).lines().len(), 6);
    assert_eq!(cell_of(&added), Some((1, 0)));
}

/// Joining across a cell boundary would merge two cells and leave their
/// rows short, so a deletion that reaches one stops there: the cell is
/// isolating, so nothing in the chain joins, and an edit that would is
/// refused by core's table invariant.
#[test]
fn backspace_stops_at_a_cell_boundary_but_still_deletes_inside_one() {
    let (state, markdown) = table_state();
    let types = types_of(&state);
    let lines = projection_of(&state);
    let (start, end) = (lines.lines()[1].from(), lines.lines()[1].to());
    let stopped = applied(&at(&state, start), &backspace(&types));
    let unchanged = |after: &Option<EditorState>| {
        after
            .as_ref()
            .is_none_or(|after| to_markdown(state.schema(), after.doc()) == markdown)
    };
    assert!(unchanged(&stopped), "nothing joins across the cell");
    if let Some(stopped) = &stopped {
        assert_eq!(cell_of(stopped), Some((0, 1)), "and the caret stays put");
    }
    // Forward delete stops at the other edge of the same cell.
    assert!(unchanged(&applied(
        &at(&state, end),
        &delete_forward(&types)
    )));
    // Inside the cell both still take a character.
    let deleted = applied(&at(&state, end), &backspace(&types)).expect("a grapheme goes");
    assert_ne!(to_markdown(state.schema(), deleted.doc()), markdown);
    let deleted = applied(&at(&state, start), &delete_forward(&types)).expect("one goes");
    assert_ne!(to_markdown(state.schema(), deleted.doc()), markdown);
    // A selection reaching out of the cell is refused outright.
    let across = state
        .update([TransactionSpec::new().selection(Selection::text(
            lines.lines()[0].from(),
            lines.lines()[1].to(),
        ))])
        .expect("a selection")
        .state()
        .clone();
    let refused = applied(&across, &backspace(&types)).expect("the invariant refuses");
    assert_eq!(to_markdown(state.schema(), refused.doc()), markdown);
    let typed = applied(&across, &insert_plain(&types, "x")).expect("the invariant refuses");
    assert_eq!(to_markdown(state.schema(), typed.doc()), markdown);
}

/// Backspace at the start of a table nobody has typed in yet takes the
/// table, which is the one case where it means the grid and not a letter.
#[test]
fn backspace_at_the_start_of_an_empty_table_takes_it() {
    let state = state_of("|   |   |\n| - | - |");
    let types = types_of(&state);
    let start = projection_of(&state).lines()[0].from();
    let taken = applied(&at(&state, start), &backspace(&types)).expect("the table goes");
    assert_eq!(to_markdown(state.schema(), taken.doc()), "");
}

#[test]
fn a_block_type_toggles_back_to_a_paragraph() {
    let state = state_of("text");
    let types = types_of(&state);
    let heading = state.schema().node_id(md::HEADING).unwrap();
    let level = Attrs::from_pairs([("level", 1i64)]);
    let command = toggle_block(&types, heading, level.clone());
    assert_eq!(after(&state, &command).as_deref(), Some("# text"));
    let state = state_of("# text");
    assert_eq!(after(&state, &command).as_deref(), Some("text"));
    // A different level sets rather than clears.
    let two = toggle_block(&types, heading, Attrs::from_pairs([("level", 2i64)]));
    assert_eq!(after(&state, &two).as_deref(), Some("## text"));
}

#[test]
fn backspace_at_heading_start_makes_a_paragraph_and_hash_promotes() {
    for heading in ["# title", "## title", "###### title"] {
        let state = state_of(heading);
        let types = types_of(&state);
        let start = projection_of(&state).lines()[0].from();
        let cleared = applied(&at(&state, start), &backspace(&types)).expect("clears");
        assert_eq!(
            to_markdown(state.schema(), cleared.doc()),
            "title",
            "{heading:?}"
        );
    }
    let state = state_of("# title");
    let types = types_of(&state);
    let start = projection_of(&state).lines()[0].from();
    let promoted = applied(&at(&state, start), &insert_plain(&types, "#")).expect("promotes");
    assert_eq!(to_markdown(state.schema(), promoted.doc()), "## title");
}

/// Typora 1.14.10: a heading that opens an item or a quote keeps its
/// level and loses the container, as a paragraph there would; anywhere
/// else it becomes a paragraph.
#[test]
fn backspace_at_a_heading_opening_a_container_takes_the_container() {
    for (source, line, expected) in [
        ("- # 1", 0, "# 1"),
        ("> # 1", 0, "# 1"),
        ("- [ ] # 1", 0, "# 1"),
        // Typora leaves the list loose; a heading needs no blank line
        // to stay apart, so the two read the same.
        ("- [ ] 0\n- [ ] # 1", 1, "- [ ] 0\n  # 1"),
        ("> 0\n>\n> # 1", 1, "> 0\n>\n> 1"),
        ("- 0\n\n  # 1", 1, "- 0\n\n  1"),
    ] {
        let state = state_of(source);
        let types = types_of(&state);
        let start = projection_of(&state).lines()[line].from();
        let after = applied(&at(&state, start), &backspace(&types)).expect("applies");
        assert_eq!(
            to_markdown(state.schema(), after.doc()),
            expected,
            "{source:?}"
        );
    }
}

#[test]
fn backspace_at_quote_start_lifts() {
    let state = state_of("> quoted");
    let types = types_of(&state);
    let start = projection_of(&state).lines()[0].from();
    let lifted = applied(&at(&state, start), &backspace(&types)).expect("lifts");
    assert_eq!(to_markdown(state.schema(), lifted.doc()), "quoted");
}

#[test]
fn a_list_toggles_off_and_switches_item_kind_in_place() {
    let state = state_of("text");
    let types = types_of(&state);
    let bullet = state.schema().node_id(md::BULLET_LIST).unwrap();
    let item = state.schema().node_id(md::LIST_ITEM).unwrap();
    let task = state.schema().node_id(md::TASK_ITEM).unwrap();
    let bullets = toggle_list(&types, bullet, Attrs::empty(), item);
    assert_eq!(after(&state, &bullets).as_deref(), Some("- text"));
    let state = state_of("- text");
    assert_eq!(after(&state, &bullets).as_deref(), Some("text"));
    // The same list with the other item kind converts in place.
    let tasks = toggle_list(&types, bullet, Attrs::empty(), task);
    assert_eq!(after(&state, &tasks).as_deref(), Some("- [ ] text"));
}

#[test]
fn a_new_list_carries_the_attributes_it_is_given() {
    let state = state_of("text");
    let types = types_of(&state);
    let bullet = state.schema().node_id(md::BULLET_LIST).unwrap();
    let item = state.schema().node_id(md::LIST_ITEM).unwrap();
    let stars = toggle_list(
        &types,
        bullet,
        markraft_core::attrs! {"bullet_char" => "*"},
        item,
    );
    assert_eq!(after(&state, &stars).as_deref(), Some("* text"));
}

#[test]
fn a_paragraph_can_be_wrapped_directly_in_a_task_list() {
    let state = state_of("text");
    let types = types_of(&state);
    let command = toggle_list(
        &types,
        types.bullet_list.unwrap(),
        Attrs::empty(),
        types.task_item.unwrap(),
    );
    assert_eq!(after(&state, &command).as_deref(), Some("- [ ] text"));
    let state = state_of("one\n\ntwo");
    let state = state
        .update([TransactionSpec::new().selection(Selection::All)])
        .unwrap()
        .state()
        .clone();
    assert_eq!(
        after(&state, &command).as_deref(),
        Some("- [ ] one\n- [ ] two")
    );
}

#[test]
fn enter_in_an_empty_completed_task_leaves_the_list() {
    let state = state_of("- [x] ");
    let state = at(&state, projection_of(&state).lines()[0].from());
    assert_eq!(
        after(&state, &enter(&types_of(&state))).as_deref(),
        Some("")
    );
}

#[test]
fn the_task_box_toggles_and_a_code_block_is_left_instead() {
    let state = state_of("- [ ] text");
    let types = types_of(&state);
    let state = at(&state, caret_in(&state, "text"));
    assert_eq!(
        after(&state, &toggle_task(&types)).as_deref(),
        Some("- [x] text")
    );
    // Outside a task item the same key leaves a code block: a new, empty
    // block after it. Empty paragraphs have no Markdown spelling.
    let state = state_of("```\ncode\n```");
    let end = projection_of(&state).lines()[0].to();
    let state = at(&state, end);
    assert_eq!(
        after(&state, &toggle_task(&types_of(&state))).as_deref(),
        Some("```\ncode\n```")
    );
}

/// The caret at `offset` characters into the document's first line.
fn offset_in_first_line(state: &EditorState, offset: usize) -> EditorState {
    let line = projection_of(state).lines()[0].clone();
    at(
        state,
        line.offset_to_pos(offset).expect("an offset in the line"),
    )
}

/// A raw block holds its source as text, so Enter writes the line ending it
/// looks like rather than splitting the block in two.
#[test]
fn enter_in_a_raw_block_writes_a_newline_and_keeps_the_block() {
    let state = state_of("<div>\nab\n</div>");
    let raw = state.doc().child(0).type_id();
    let state = offset_in_first_line(&state, "<div>\na".chars().count());
    let split = applied(&state, &enter(&types_of(&state))).expect("Enter applies");
    assert_eq!(
        to_markdown(state.schema(), split.doc()),
        "<div>\na\nb\n</div>"
    );
    assert_eq!(split.doc().child_count(), 1, "still one block");
    assert_eq!(split.doc().child(0).type_id(), raw, "and still the raw one");
}

/// Typing past the last character of a raw block stays in it: there is no
/// chrome to fall out of, so the text simply grows.
#[test]
fn typing_at_the_end_of_a_raw_block_stays_inside_it() {
    let state = state_of("<div>\nab\n</div>");
    let raw = state.doc().child(0).type_id();
    let state = at(&state, projection_of(&state).lines()[0].to());
    let typed =
        applied(&state, &insert_plain(&types_of(&state), "\nc")).expect("the insertion applies");
    assert_eq!(
        to_markdown(state.schema(), typed.doc()),
        "<div>\nab\n</div>\nc"
    );
    assert_eq!(typed.doc().child_count(), 1, "the newline stayed literal");
    assert_eq!(typed.doc().child(0).type_id(), raw);
}

/// An emptied raw block draws nothing at all, so Backspace — the key that
/// would otherwise delete nothing — is the way out of one.
#[test]
fn backspace_in_an_empty_raw_block_leaves_a_paragraph() {
    let state = state_of("<div>");
    let types = types_of(&state);
    let line = projection_of(&state).lines()[0].clone();
    let emptied = applied(&state, &delete_range(line.from(), line.to())).expect("the text goes");
    let emptied = at(&emptied, projection_of(&emptied).lines()[0].from());
    let cleared = applied(&emptied, &backspace(&types)).expect("Backspace applies");
    assert_eq!(cleared.doc().child_count(), 1);
    assert_eq!(Some(cleared.doc().child(0).type_id()), types.paragraph);
    // A raw block with text in it still loses one character at a time.
    let state = at(&state, line.to());
    let deleted = applied(&state, &backspace(&types)).expect("a grapheme goes");
    assert_eq!(to_markdown(state.schema(), deleted.doc()), "<div");
}

/// An emptied raw block after another block goes in one press, as an
/// empty paragraph would, leaving the caret where the block before ends.
/// An empty code block is still only turned into a paragraph.
#[test]
fn backspace_in_an_empty_raw_block_after_a_block_deletes_it() {
    let state = state_of("a\n\n[r]: https://x.y");
    let types = types_of(&state);
    let line = projection_of(&state).lines()[1].clone();
    let emptied = applied(&state, &delete_range(line.from(), line.to())).expect("the text goes");
    let emptied = at(&emptied, projection_of(&emptied).lines()[1].from());
    let joined = applied(&emptied, &backspace(&types)).expect("Backspace applies");
    assert_eq!(to_markdown(state.schema(), joined.doc()), "a");
    assert_eq!(joined.doc().child_count(), 1);
    assert_eq!(Some(joined.doc().child(0).type_id()), types.paragraph);
    let end = projection_of(&joined).lines()[0].to();
    assert_eq!(joined.selection().head(joined.doc()), end);

    let state = state_of("a\n\n```\n```");
    let state = at(&state, projection_of(&state).lines()[1].from());
    let cleared = applied(&state, &backspace(&types_of(&state))).expect("Backspace applies");
    assert_eq!(cleared.doc().child_count(), 2);
    assert_eq!(Some(cleared.doc().child(1).type_id()), types.paragraph);
}

#[test]
fn literal_text_splits_into_blocks_but_stays_literal_in_code() {
    let state = state_of("");
    let types = types_of(&state);
    assert_eq!(
        after(&state, &insert_plain(&types, "one\ntwo")).as_deref(),
        Some("one\n\ntwo")
    );
    let state = state_of("```\n\n```");
    let state = at(&state, projection_of(&state).lines()[0].to());
    assert_eq!(
        after(&state, &insert_plain(&types_of(&state), "a\nb")).as_deref(),
        Some("```\na\nb\n```")
    );
}

/// Return in the last item of a list, then Backspace twice: the first
/// joins the empty item to the one before as an empty paragraph, as Typora
/// does, the second joins that paragraph back and leaves the caret where it
/// started — not in the list that follows.
#[test]
fn backspace_twice_from_a_new_last_item_returns_to_the_item_before() {
    let state = state_of("1. eight\n9. nine\n\n- bullet a");
    let types = types_of(&state);
    let end_of_nine = caret_in(&state, "nine") + 4;
    let state = applied(&at(&state, end_of_nine), &enter(&types)).expect("Return applies");
    let joined = applied(&state, &backspace(&types)).expect("Backspace joins the item");
    assert_eq!(
        joined.doc().child(0).child_count(),
        2,
        "the empty item joined"
    );
    let back = applied(&joined, &backspace(&types)).expect("Backspace joins the paragraph");
    assert_eq!(back.doc().child_count(), 2);
    assert_eq!(back.selection().head(back.doc()), end_of_nine);
    let typed = applied(&back, &markraft_core::commands::insert_text("5")).expect("typing");
    assert_eq!(
        to_markdown(typed.schema(), typed.doc()),
        "1. eight\n2. nine5\n\n- bullet a"
    );
}

/// Return at the end of `eight` in the middle of an ordered list, then
/// Backspace: the new empty item joins `eight` as an empty paragraph, and
/// what is typed next is a paragraph of that item, as Typora does. The
/// empty paragraph writes nothing, so the list is saved as it was until then.
#[test]
fn backspace_in_an_empty_middle_item_joins_the_item_before() {
    let source = "7. seven\n8. eight\n9. nine\n";
    let state = state_of(source);
    let types = types_of(&state);
    let end_of_eight = caret_in(&state, "eight") + 5;
    let state = applied(&at(&state, end_of_eight), &enter(&types)).expect("Return applies");
    let joined = applied(&state, &backspace(&types)).expect("Backspace joins the item");
    assert_eq!(
        joined.doc().child(0).child_count(),
        3,
        "the empty item joined"
    );
    let saved =
        markraft_commonmark::SourceDocument::parse(joined.schema(), source).expect("parses");
    assert_eq!(
        saved.render(joined.schema(), joined.doc()).as_deref(),
        Ok(source)
    );
    let typed = applied(&joined, &markraft_core::commands::insert_text("5")).expect("typing");
    assert_eq!(
        to_markdown(typed.schema(), typed.doc()),
        "7. seven\n\n8. eight\n\n   5\n\n9. nine"
    );
}

#[test]
fn backspace_in_an_empty_middle_bullet_or_task_item_joins_the_item_before() {
    for (source, line, expected) in [
        (
            "- one\n- two\n- three",
            "two",
            "- one\n\n- two\n\n  5\n\n- three",
        ),
        (
            "- [ ] one\n- [ ] two\n- [ ] three",
            "two",
            "- [ ] one\n\n- [ ] two\n\n  5\n\n- [ ] three",
        ),
    ] {
        let state = state_of(source);
        let types = types_of(&state);
        let end = caret_in(&state, line) + line.len();
        let state = applied(&at(&state, end), &enter(&types)).expect("Return applies");
        let joined = applied(&state, &backspace(&types)).expect("Backspace joins the item");
        let typed = applied(&joined, &markraft_core::commands::insert_text("5")).expect("typing");
        assert_eq!(
            to_markdown(typed.schema(), typed.doc()),
            expected,
            "{source}"
        );
    }
}

/// Delete at the end of a textblock and Backspace at the start of one right
/// after a list or a quote join the two textblocks' text wherever they sit,
/// as Typora does. A code block keeps its text to itself.
#[test]
fn delete_and_backspace_join_text_across_lists_and_quotes() {
    let forward = [
        ("- one\n- two", "one", "- onetwo"),
        ("zero\n\n- one", "zero", "zeroone"),
        ("- one\n\ntwo", "one", "- onetwo"),
        ("- one\n  - two", "one", "- onetwo"),
        ("> one\n\ntwo", "one", "> onetwo"),
    ];
    for (source, line, expected) in forward {
        let state = state_of(source);
        let state = at(&state, caret_in(&state, line) + line.len());
        let joined = after(&state, &delete_forward(&types_of(&state)));
        assert_eq!(joined.as_deref(), Some(expected), "Delete in {source:?}");
    }
    let backward = [
        ("- one\n\ntwo", "two", "- onetwo"),
        ("> one\n\ntwo", "two", "> onetwo"),
        ("- one\n  - two\n\nthree", "three", "- one\n  - twothree"),
    ];
    for (source, line, expected) in backward {
        let state = state_of(source);
        let state = at(&state, caret_in(&state, line));
        let joined = after(&state, &backspace(&types_of(&state)));
        assert_eq!(joined.as_deref(), Some(expected), "Backspace in {source:?}");
    }
    // Before a code block or a table, Delete does nothing.
    for source in ["one\n\n```\ncode\n```", "one\n\n| a |\n| - |\n| b |"] {
        let state = state_of(source);
        let state = at(&state, caret_in(&state, "one") + 3);
        assert_eq!(
            after(&state, &delete_forward(&types_of(&state))),
            None,
            "{source:?}"
        );
    }
}

/// Around a divider, Backspace and Delete take it at once, as Typora does,
/// rather than first selecting it: Backspace leaves the caret where it was,
/// and Delete carries the text after the divider on the line it ends.
#[test]
fn backspace_and_delete_take_a_divider_at_once() {
    let state = state_of("one\n\n---\n\ntwo");
    let types = types_of(&state);
    let back = applied(&at(&state, caret_in(&state, "two")), &backspace(&types))
        .expect("Backspace takes the divider");
    assert_eq!(to_markdown(back.schema(), back.doc()), "one\n\ntwo");
    assert_eq!(
        back.selection().head(back.doc()),
        caret_in(&back, "two"),
        "the caret stays at the start of the line"
    );
    let forward = applied(
        &at(&state, caret_in(&state, "one") + 3),
        &delete_forward(&types),
    )
    .expect("Delete takes the divider");
    assert_eq!(to_markdown(forward.schema(), forward.doc()), "onetwo");
    assert_eq!(
        forward.selection().head(forward.doc()),
        caret_in(&forward, "onetwo") + 3
    );
    // With nothing to join after it, Delete still takes the divider.
    let last = state_of("one\n\n---");
    let gone = after(
        &at(&last, caret_in(&last, "one") + 3),
        &delete_forward(&types_of(&last)),
    );
    assert_eq!(gone.as_deref(), Some("one"));
}

/// → leaves a selected divider for the line after it, and makes one when
/// the divider ends the note; ← goes back to the line before.
#[test]
fn arrows_leave_a_selected_divider() {
    for (source, expected) in [
        ("one\n\n---\n\ntwo", "one\n\n---\n\ntwo"),
        ("one\n\n---", "one\n\n---\n\n"),
    ] {
        let state = state_of(source);
        let rule = projection_of(&state).lines()[1].from();
        let selected = state
            .update([TransactionSpec::new().selection(Selection::node(rule))])
            .expect("the divider is selectable")
            .state()
            .clone();
        let right = applied(&selected, &move_grapheme(Direction::Forward, false))
            .expect("→ leaves the divider");
        assert!(right.selection().is_cursor(), "{source:?}");
        assert_eq!(
            to_markdown(right.schema(), right.doc()),
            expected.trim_end()
        );
        assert!(
            right.selection().head(right.doc()) > rule,
            "{source:?}: the caret is past the divider"
        );
        let left = applied(&selected, &move_grapheme(Direction::Backward, false))
            .expect("← leaves the divider");
        assert_eq!(
            left.selection(),
            &Selection::cursor(caret_in(&state, "one") + 3)
        );
    }
}

/// ← from the start of the line after a divider and → from the end of the
/// line before it select the divider, as ↑ and ↓ do; with Shift, the
/// selection grows over it to the text beyond. No caret is left beside it,
/// where there is no line to type in.
#[test]
fn arrows_onto_a_divider_select_it() {
    let state = state_of("one\n\n---\n\ntwo");
    let rule = projection_of(&state).lines()[1].from();
    let end_of_one = caret_in(&state, "one") + 3;
    let start_of_two = caret_in(&state, "two");
    for (from, dir) in [
        (start_of_two, Direction::Backward),
        (end_of_one, Direction::Forward),
    ] {
        let moved =
            applied(&at(&state, from), &move_grapheme(dir, false)).expect("the arrow moves");
        assert_eq!(moved.selection(), &Selection::node(rule), "{dir:?}");
    }
    let grown = applied(
        &at(&state, end_of_one),
        &move_grapheme(Direction::Forward, true),
    )
    .expect("Shift-→ extends");
    assert_eq!(
        grown.selection(),
        &Selection::text(end_of_one, start_of_two)
    );
}

/// Backspace at the start of a paragraph right after a table carries its
/// text into the table's last cell, as Typora does.
#[test]
fn backspace_after_a_table_joins_its_last_cell() {
    let state = state_of("| a | b |\n| - | - |\n| c | d |\n\ntwo");
    let state = at(&state, caret_in(&state, "two"));
    let joined = applied(&state, &backspace(&types_of(&state))).expect("the join applies");
    let markdown = to_markdown(joined.schema(), joined.doc());
    assert!(markdown.contains("| dtwo"), "{markdown}");
    assert_eq!(joined.doc().child_count(), 1, "the paragraph is gone");
    assert_eq!(
        cell_of(&joined),
        Some((1, 1)),
        "the caret is in the last cell"
    );
}

/// Backspace at the start of a quote's later paragraph joins the paragraph
/// before it in the same quote, as Typora does; only the quote's first
/// block lifts out of it.
#[test]
fn backspace_in_a_quotes_later_paragraph_joins_the_one_before() {
    let state = state_of("> one\n>\n> two");
    let state = at(&state, caret_in(&state, "two"));
    let joined = after(&state, &backspace(&types_of(&state)));
    assert_eq!(joined.as_deref(), Some("> onetwo"));
    let first = state_of("> one\n>\n> two");
    let lifted = after(
        &at(&first, caret_in(&first, "one")),
        &backspace(&types_of(&first)),
    );
    assert_eq!(lifted.as_deref(), Some("one\n\n> two"));
}

/// The first item of a list has nothing before it to return to: an empty
/// one leaves the list as before.
#[test]
fn backspace_in_an_empty_first_item_still_leaves_the_list() {
    let state = state_of("- \n- two");
    let types = types_of(&state);
    let state = at(&state, projection_of(&state).lines()[0].from());
    let lifted = applied(&state, &backspace(&types)).expect("Backspace lifts the item");
    assert_eq!(lifted.doc().child_count(), 2, "a paragraph before the list");
}

/// Return at the start of a list's first item, ↑ into the empty item it
/// leaves, Backspace to lift it out, then type: the file saved from the
/// original source has one blank line either side of the new paragraph,
/// for an ordered list and a bullet list alike. The last item, which has an
/// item before it, joins that one instead, as Typora does: what is typed is
/// a paragraph of the item before, and the list is written loose.
#[test]
fn typing_into_a_lifted_first_item_saves_one_blank_line_either_side() {
    use markraft_commonmark::SourceDocument;
    for (source, line, at_start, expected) in [
        (
            "# Lists\n\n1. one\n2. two\n",
            "one",
            true,
            "# Lists\n\n5\n\n1. one\n2. two\n",
        ),
        (
            "# Lists\n\n- one\n- two\n",
            "one",
            true,
            "# Lists\n\n5\n\n- one\n- two\n",
        ),
        (
            "# Lists\n\n1. eight\n9. nine\n",
            "nine",
            false,
            "# Lists\n\n1. eight\n\n2. nine\n\n   5\n",
        ),
    ] {
        let state = state_of(source);
        let types = types_of(&state);
        let caret = caret_in(&state, line) + if at_start { 0 } else { line.len() };
        let state = applied(&at(&state, caret), &enter(&types)).expect("Return applies");
        let empty = projection_of(&state)
            .lines()
            .iter()
            .find(|line| line.is_empty())
            .expect("an empty item")
            .from();
        let lifted =
            applied(&at(&state, empty), &backspace(&types)).expect("Backspace lifts the item");
        let typed = applied(&lifted, &markraft_core::commands::insert_text("5")).expect("typing");
        let saved = SourceDocument::parse(typed.schema(), source).expect("the source parses");
        assert_eq!(
            saved.render(typed.schema(), lifted.doc()).as_deref(),
            Ok(source),
            "the empty paragraph alone changes nothing on disk: {source:?}"
        );
        assert_eq!(
            saved.render(typed.schema(), typed.doc()).as_deref(),
            Ok(expected),
            "{source:?}"
        );
    }
}

/// Word motion steps over what a reader sees: never between the two `*`
/// of a hidden `**`, and over a hidden span's delimiters to the word they
/// open, so what is typed there lands outside the span.
#[test]
fn word_motion_steps_over_hidden_delimiters() {
    let state = state_of("hello **world** end");
    let types = types_of(&state);
    let end = caret_in(&state, "hello **world** end") + "hello **world** end".len();
    let back = move_word(&types, Direction::Backward, false);
    let once = applied(&at(&state, end), &back).expect("⌥← applies");
    let twice = applied(&once, &back).expect("⌥← applies again");
    let typed = markraft_core::commands::insert_text("X");
    assert_eq!(
        after(&once, &typed).as_deref(),
        Some("hello **world** Xend")
    );
    assert_eq!(
        after(&twice, &typed).as_deref(),
        Some("hello X**world** end")
    );

    let start = caret_in(&state, "hello **world** end") + "hello".len();
    let forward = move_word(&types, Direction::Forward, false);
    let moved = applied(&at(&state, start), &forward).expect("⌥→ applies");
    assert_eq!(
        after(&moved, &typed).as_deref(),
        Some("hello **world**X end")
    );
}

/// A line that is one hidden span still has a word inside it: ⌥← from its
/// end reaches the word's start, ⌥→ from its start the word's end, as on a
/// line with plain text around the span.
#[test]
fn word_motion_reaches_a_span_that_fills_its_line() {
    let typed = markraft_core::commands::insert_text("X");
    // The caret at the span's edge shows its markup, so the word it reaches
    // is inside the span, as Typora 1.14.10 has it.
    for (source, expected) in [
        ("**abc**", "**Xabc**"),
        ("`abc`", "`Xabc`"),
        ("**abc** d", "**abc** Xd"),
    ] {
        let state = state_of(source);
        let types = types_of(&state);
        let end = caret_in(&state, source) + source.len();
        let back = move_word(&types, Direction::Backward, false);
        let moved = applied(&at(&state, end), &back).expect("⌥← applies");
        assert_eq!(
            after(&moved, &typed).as_deref(),
            Some(expected),
            "{source:?}"
        );
    }
    let state = state_of("**abc**");
    let types = types_of(&state);
    let start = caret_in(&state, "**abc**");
    let forward = move_word(&types, Direction::Forward, false);
    let moved = applied(&at(&state, start), &forward).expect("⌥→ applies");
    assert_eq!(after(&moved, &typed).as_deref(), Some("**abcX**"));
}

/// ⌥⌫ deletes a hidden span whole rather than one of its delimiter
/// characters at a time.
#[test]
fn word_deletion_takes_a_hidden_span_whole() {
    let state = state_of("hello **world** end");
    let types = types_of(&state);
    let end = caret_in(&state, "hello **world** end") + "hello **world** end".len();
    let delete = delete_word(&types, Direction::Backward);
    let once = applied(&at(&state, end), &delete).expect("⌥⌫ applies");
    let twice = applied(&once, &delete).expect("⌥⌫ applies again");
    let typed = markraft_core::commands::insert_text("X");
    assert_eq!(after(&once, &typed).as_deref(), Some("hello **world** X"));
    assert_eq!(after(&twice, &typed).as_deref(), Some("hello X"));
}

/// Inside a revealed span its delimiters are text a reader sees, but a run
/// of them is still one stop, not one per character.
#[test]
fn word_motion_never_stops_inside_a_revealed_delimiter_run() {
    let state = state_of("a **bc** d");
    let types = types_of(&state);
    let inside = caret_in(&state, "a **bc** d") + "a **bc".len();
    let forward = move_word(&types, Direction::Forward, false);
    let moved = applied(&at(&state, inside), &forward).expect("⌥→ applies");
    let typed = markraft_core::commands::insert_text("X");
    // Nor at its end: the run is skipped to the next word's end.
    assert_eq!(after(&moved, &typed).as_deref(), Some("a **bc** dX"));
}

/// Shift-Return breaks the line inside the block: a hard break, spelled
/// with the trailing `\` the file keeps, and a newline in a code block.
#[test]
fn shift_return_breaks_the_line_inside_the_block() {
    let typed = markraft_core::commands::insert_text("b");
    let state = state_of("a");
    let types = types_of(&state);
    let broken = applied(
        &at(&state, caret_in(&state, "a") + 1),
        &line_break(&types, &PlainKind),
    )
    .expect("Shift-Return applies");
    assert_eq!(after(&broken, &typed).as_deref(), Some("a\\\nb"));
    // Left with nothing after it, the break goes, and its `\` with it.
    let state = state_of("a\n\nz");
    let broken = applied(
        &at(&state, caret_in(&state, "a") + 1),
        &line_break(&types, &PlainKind),
    )
    .expect("Shift-Return applies");
    let left = at(&broken, caret_in(&broken, "z"));
    assert_eq!(to_markdown(left.schema(), left.doc()), "a\n\nz");

    let state = state_of("**ab**");
    let broken = applied(
        &at(&state, caret_in(&state, "**ab**") + 3),
        &line_break(&types, &PlainKind),
    )
    .expect("Shift-Return applies inside a span");
    assert_eq!(to_markdown(broken.schema(), broken.doc()), "**a\\\nb**");

    let state = state_of("```\nx\n```");
    let broken = applied(
        &at(&state, caret_in(&state, "x") + 1),
        &line_break(&types, &PlainKind),
    )
    .expect("Shift-Return applies in code");
    assert_eq!(after(&broken, &typed).as_deref(), Some("```\nx\nb\n```"));

    // A GFM row is one line: in a table cell the break is `<br />`, as
    // Typora writes it.
    let (state, _) = table_state();
    let caret = caret_in(&state, "c") + 1;
    let broken = applied(&at(&state, caret), &line_break(&types, &PlainKind))
        .expect("Shift-Return applies in a cell");
    let markdown = to_markdown(broken.schema(), broken.doc());
    assert!(markdown.contains("c<br />"), "{markdown:?}");
}

/// A kind that spells hard breaks with two spaces gets them from
/// Shift-Return, and a break already spelled with `\` keeps it.
#[test]
fn shift_return_spells_the_break_as_the_kind_says() {
    /// A kind that spells a hard break with two spaces.
    struct Spaces;
    impl DocumentKind for Spaces {
        fn break_spelling(&self) -> Option<&'static str> {
            Some("  ")
        }
    }
    let typed = markraft_core::commands::insert_text("c");
    let state = state_of("x\\\ny\n\nb");
    let types = types_of(&state);
    let command = line_break(&types, &Spaces);
    let broken =
        applied(&at(&state, caret_in(&state, "b") + 1), &command).expect("Shift-Return applies");
    assert_eq!(after(&broken, &typed).as_deref(), Some("x\\\ny\n\nb  \nc"));
    // Left with nothing after it, the break goes, and its spaces with it.
    let state = state_of("a\n\nz");
    let broken =
        applied(&at(&state, caret_in(&state, "a") + 1), &command).expect("Shift-Return applies");
    let left = at(&broken, caret_in(&broken, "z"));
    assert_eq!(to_markdown(left.schema(), left.doc()), "a\n\nz");
}

/// Letting the caret into a picture's source is not an edit of its own:
/// one undo takes back what was typed before it, and gives the picture
/// back as the atom it was.
#[test]
fn letting_the_caret_into_a_picture_is_not_an_undo_step() {
    let state = state_of("after\n\n![a](x.png)");
    let typed = applied(
        &at(&state, caret_in(&state, "after") + "after".len()),
        &markraft_core::commands::insert_text("z"),
    )
    .expect("typing");
    let picture = projection_of(&typed).lines()[1].from();
    let reached = applied(
        &typed,
        &markraft_core::commands::command(move |_| {
            Some(TransactionSpec::new().selection(Selection::cursor(picture)))
        }),
    )
    .expect("the caret moves");
    assert_eq!(
        projection_of(&reached).line_text(1),
        Some("![a](x.png)"),
        "the caret found the source"
    );
    let undone = applied(&reached, &history(true)).expect("undo applies");
    let schema = undone.schema();
    assert_eq!(to_markdown(schema, undone.doc()), "after\n\n![a](x.png)");
    assert_eq!(undone.doc(), state.doc(), "the picture is the atom again");
}
