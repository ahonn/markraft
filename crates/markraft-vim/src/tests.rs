//! The whole of vim driven over a bare [`EditorState`].
//!
//! [`Keys`] implements the same [`Host`] the GPUI layer implements and dispatches a
//! keystroke to the same `command` functions the action handlers call, so these tests
//! exercise the real command layer rather than a copy of it. Only two inputs cannot be
//! reproduced without a window: `j` and `k` outside a linewise context move by visual
//! row, which here is one line, and a repaint.
//!
//! A cursor is a document position; the assertions spell it as a line and a `char`
//! offset into that line, which inside a line is the same number.

use crate::{
    command::{self, InsertAt},
    host::{self, Host},
    motion::Motion,
    state::{Mode, Operator, State},
};
use markraft_commonmark::{
    commonmark_doc_type_names, commonmark_extensions, commonmark_schema, from_markdown, to_markdown,
};
use markraft_core::commands::Command;
use markraft_core::projection::{Projection, projection_of, slice_to_plain_text};
use markraft_core::{EditorState, EditorStateConfig, Extension, Selection, Slice, TransactionSpec};
use markraft_gpui::DocTypes;
use std::sync::Arc;

/// A state plus a clipboard, standing in for `EditorCx`.
struct Editing {
    state: EditorState,
    types: DocTypes,
    clipboard: Option<Slice>,
    group_depth: usize,
}

impl Host for Editing {
    fn state(&self) -> &EditorState {
        &self.state
    }
    fn types(&self) -> &DocTypes {
        &self.types
    }
    fn projection(&self) -> Arc<Projection> {
        projection_of(&self.state)
    }
    fn plain_text(&self, slice: &Slice) -> String {
        markraft_commonmark::slice_to_plain_text(self.state.schema(), slice)
    }
    fn select(&mut self, selection: Selection, _: bool) {
        self.dispatch(vec![TransactionSpec::new().selection(selection)]);
    }
    fn dispatch(&mut self, specs: Vec<TransactionSpec>) -> bool {
        match self.state.update_with_appended(specs) {
            Ok(transactions) => match transactions.last() {
                Some(last) => {
                    self.state = last.state().clone();
                    true
                }
                None => false,
            },
            Err(_) => false,
        }
    }
    fn run(&mut self, command: &Command) -> bool {
        match command(&self.state) {
            Some(spec) => self.dispatch(vec![spec]),
            None => false,
        }
    }
    /// No layout, so a visual row is a line; the column is kept as a `char` offset,
    /// which is what an unwrapped line would do anyway.
    fn rows(&mut self, rows: isize, extend: bool) {
        let projection = self.projection();
        let head = host::head(self);
        let anchor = host::anchor(self);
        let (line, offset) = projection.pos_to_line_offset(head).unwrap_or((0, 0));
        let last = projection.line_count().saturating_sub(1);
        let target = (line as isize).saturating_add(rows).clamp(0, last as isize) as usize;
        let entry = &projection.lines()[target];
        let head = entry
            .offset_to_pos(offset.min(entry.len()))
            .expect("a visible offset");
        self.select(
            Selection::text(if extend { anchor } else { head }, head),
            false,
        );
    }
    fn write_clipboard(&mut self, slice: &Slice) {
        self.clipboard = Some(slice.clone());
    }
    fn read_clipboard(&mut self) -> Option<Slice> {
        self.clipboard.clone()
    }
    fn history(&mut self, undo: bool) -> bool {
        let spec = if undo {
            markraft_core::undo(&self.state)
        } else {
            markraft_core::redo(&self.state)
        };
        match spec {
            Some(spec) => self.dispatch(vec![spec]),
            None => false,
        }
    }
    fn begin_undo_group(&mut self) {
        self.group_depth += 1;
        self.dispatch(vec![
            TransactionSpec::new()
                .effect(markraft_core::begin_undo_group().of(()))
                .add_to_history(false),
        ]);
    }
    fn end_undo_group(&mut self) {
        while self.group_depth > 0 {
            self.group_depth -= 1;
            self.dispatch(vec![
                TransactionSpec::new()
                    .effect(markraft_core::end_undo_group().of(()))
                    .add_to_history(false),
            ]);
        }
    }
}

/// A vim session: the same state the extension keeps, and the same dispatch its action
/// handlers perform, one key at a time.
struct Keys {
    host: Editing,
    state: State,
}

impl Keys {
    fn new(markdown: &str) -> Self {
        let schema = commonmark_schema();
        let doc = from_markdown(&schema, markdown).expect("valid Markdown");
        let state =
            EditorState::create(EditorStateConfig::new(schema.clone()).doc(doc).extensions(
                Extension::all([
                    markraft_core::projection::projection(),
                    markraft_core::composition(),
                    markraft_core::history(Default::default()),
                    commonmark_extensions(&schema),
                ]),
            ))
            .expect("a valid state");
        Self {
            host: Editing {
                types: DocTypes::from_schema_names(&schema, &commonmark_doc_type_names()),
                state,
                clipboard: None,
                group_depth: 0,
            },
            state: State::default(),
        }
    }

    /// A document of plain paragraphs holding exactly `lines`, for the cases where
    /// the Markdown reader would strip the leading whitespace a test needs.
    fn lines_of(lines: &[&str]) -> Self {
        let schema = commonmark_schema();
        let blocks: Vec<_> = lines
            .iter()
            .map(|text| {
                let content = if text.is_empty() {
                    Vec::new()
                } else {
                    vec![schema.text(text)]
                };
                schema
                    .node("paragraph", content)
                    .expect("text is valid paragraph content")
            })
            .collect();
        let doc = schema.doc(blocks).expect("a valid document");
        let mut keys = Self::new("");
        keys.host.state =
            EditorState::create(EditorStateConfig::new(schema.clone()).doc(doc).extensions(
                Extension::all([
                    markraft_core::projection::projection(),
                    markraft_core::composition(),
                    markraft_core::history(Default::default()),
                    commonmark_extensions(&schema),
                ]),
            ))
            .expect("a valid state");
        keys
    }

    /// A transaction from outside vim — the platform's input handler, an
    /// extension, a click. Vim's `update` hook sees it, exactly as it does live.
    fn external(&mut self, specs: Vec<TransactionSpec>) -> &mut Self {
        self.host.dispatch(specs);
        self.settle();
        self
    }

    /// What an input method does: mark a candidate, then commit it. The specs
    /// are the ones the view's own input handler builds.
    fn composed(&mut self, text: &str) -> &mut Self {
        let head = host::head(&self.host);
        let start =
            markraft_core::start_composition(markraft_core::CompositionRange::new(head, head));
        self.external(vec![start]);
        let update =
            markraft_core::update_composition(self.host.state(), text, text.chars().count())
                .expect("a composition update");
        self.external(vec![update]);
        assert!(markraft_core::is_composing(self.host.state()));
        let types = crate::host::Host::types(&self.host).clone();
        let specs = markraft_gpui::ime::commit_specs(self.host.state(), &types, None, text);
        self.external(specs);
        assert!(!markraft_core::is_composing(self.host.state()));
        self
    }

    /// The Return key, which is the editor's own Enter chain.
    fn enter(&mut self) -> &mut Self {
        let command = markraft_gpui::commands::enter(&self.host.types.clone());
        self.host.run(&command);
        self
    }

    /// The document position at `offset` `char`s into line `line`.
    fn pos(&self, line: usize, offset: usize) -> usize {
        let projection = self.host.projection();
        let entry = &projection.lines()[line];
        entry
            .offset_to_pos(offset.min(entry.len()))
            .expect("a visible offset")
    }

    fn at(mut self, line: usize, offset: usize) -> Self {
        let pos = self.pos(line, offset);
        self.host.select(Selection::cursor(pos), false);
        self
    }

    /// Type `keys`, one grapheme per keystroke. `<esc>` is the only named key; `gg` is
    /// spelled `g` twice, as gpui's pending keystrokes deliver it.
    fn keys(&mut self, keys: &str) -> &mut Self {
        let mut rest = keys;
        while !rest.is_empty() {
            if let Some(tail) = rest.strip_prefix("<esc>") {
                self.escape();
                rest = tail;
                continue;
            }
            if let Some(tail) = rest.strip_prefix("gg") {
                let line = self.state.pending.count().map_or(0, |count| count - 1);
                command::motion(&mut self.state, &mut self.host, Motion::Line(line));
                rest = tail;
                continue;
            }
            let key = rest.chars().next().expect("a key");
            rest = &rest[key.len_utf8()..];
            self.key(key);
        }
        self.settle();
        self
    }

    fn escape(&mut self) {
        if self.state.pending.is_empty() {
            command::normal(&mut self.state, &mut self.host);
        } else {
            command::clear_pending(&mut self.state, &mut self.host);
        }
    }

    fn key(&mut self, key: char) {
        let state = &mut self.state;
        let host = &mut self.host;
        let motion = |state: &mut State, host: &mut Editing, motion| {
            command::motion(state, host, motion);
        };
        match key {
            '0'..='9' => {
                let digit = key as usize - '0' as usize;
                if !state.pending.digit(digit) {
                    motion(state, host, Motion::LineStart);
                }
            }
            'h' => motion(state, host, Motion::Left),
            'l' => motion(state, host, Motion::Right),
            'w' => motion(state, host, Motion::WordForward),
            'b' => motion(state, host, Motion::WordBackward),
            'e' => motion(state, host, Motion::WordEnd),
            '^' => motion(state, host, Motion::FirstNonBlank),
            '$' => motion(state, host, Motion::LineEnd),
            'j' => command::vertical(state, host, 1),
            'k' => command::vertical(state, host, -1),
            'G' => {
                let last = host.projection().line_count() - 1;
                let line = state.pending.count().map_or(last, |count| count - 1);
                motion(state, host, Motion::Line(line));
            }
            'd' => command::operator(state, host, Operator::Delete),
            'c' => command::operator(state, host, Operator::Change),
            'y' => command::operator(state, host, Operator::Yank),
            'x' => command::delete_chars(state, host),
            'D' => command::to_line_end(state, host, Operator::Delete),
            'C' => command::to_line_end(state, host, Operator::Change),
            'p' => command::paste(state, host, true),
            'P' => command::paste(state, host, false),
            'u' => command::history(state, host, true),
            'r' => command::history(state, host, false),
            'i' => command::insert(state, host, InsertAt::Cursor),
            'a' => command::insert(state, host, InsertAt::AfterCursor),
            'I' => command::insert(state, host, InsertAt::FirstNonBlank),
            'A' => command::insert(state, host, InsertAt::LineEnd),
            'o' => command::open_line(state, host, true),
            'O' => command::open_line(state, host, false),
            'v' => command::visual(state, host, false),
            'V' => command::visual(state, host, true),
            key => panic!("no vim binding for {key:?} in these tests"),
        }
    }

    /// What the extension's `update` hook does after every editor update.
    fn settle(&mut self) {
        if self.state.mode != Mode::Insert {
            command::settle(&mut self.state, &mut self.host, false);
        }
        self.check();
    }

    /// Text typed in Insert mode, exactly as the platform delivers it.
    fn typed(&mut self, text: &str) -> &mut Self {
        assert_eq!(self.state.mode, Mode::Insert, "typing outside Insert mode");
        command::typed(&mut self.host, text);
        self
    }

    fn cursor(&self) -> usize {
        command::cursor(&self.state, &self.host)
    }

    /// The cursor as a line index and a `char` offset into that line.
    fn line_col(&self) -> (usize, usize) {
        self.host
            .projection()
            .pos_to_line_offset(self.cursor())
            .expect("the cursor sits in a line")
    }

    /// The caret the editor actually holds, which in Insert mode may rest past the
    /// last grapheme where `cursor` would pull it back.
    fn caret(&self) -> (usize, usize) {
        self.host
            .projection()
            .pos_to_line_offset(host::head(&self.host))
            .expect("the caret sits in a line")
    }

    fn text(&self) -> String {
        self.host.projection().plain_text().to_owned()
    }

    fn markdown(&self) -> String {
        to_markdown(self.host.state.schema(), self.host.state.doc())
    }

    fn selected(&self) -> String {
        let state = &self.host.state;
        slice_to_plain_text(
            state.schema(),
            &state
                .selection()
                .content_with_schema(state.doc(), state.schema()),
        )
    }

    fn lines(&self) -> usize {
        self.host.projection().line_count()
    }

    /// Every command leaves a document the schema accepts, and every table in it as
    /// wide as its header. The second half is not something [`Node::check`] can
    /// report: a row with fewer cells than its siblings breaks no content rule, and
    /// there is no command that would put it right again.
    fn check(&self) {
        let (schema, doc) = (self.host.state.schema(), self.host.state.doc());
        doc.check(schema).expect("a valid document");
        fn walk(schema: &markraft_core::Schema, node: &markraft_core::Node) {
            if schema.node_type(node.type_id()).name() == "table" {
                let widths: Vec<usize> = node.children().map(|row| row.child_count()).collect();
                assert!(
                    widths.iter().all(|width| *width == widths[0]),
                    "a table row lost a cell: {widths:?}"
                );
            }
            node.children().for_each(|child| walk(schema, child));
        }
        walk(schema, doc);
    }
}

// ---------------------------------------------------------------- motions

#[test]
fn h_and_l_stay_inside_the_line_and_stop_on_its_last_grapheme() {
    let mut keys = Keys::new("abc\n\ndef").at(0, 1);
    assert_eq!(keys.keys("h").line_col(), (0, 0));
    assert_eq!(keys.keys("h").line_col(), (0, 0));
    assert_eq!(keys.keys("l").line_col(), (0, 1));
    // Normal mode never rests past the last grapheme, so `l` stops there.
    assert_eq!(keys.keys("lll").line_col(), (0, 2));
    assert_eq!(keys.keys("9l").line_col(), (0, 2));
    assert_eq!(keys.keys("9h").line_col(), (0, 0));
}

#[test]
fn motions_step_whole_grapheme_clusters() {
    // A family emoji, a combining acute, and CJK.
    let mut keys = Keys::new("👩‍👩‍👧e\u{0301}中").at(0, 0);
    let family = "👩‍👩‍👧".chars().count();
    let combining = "e\u{0301}".chars().count();
    assert_eq!(keys.keys("l").line_col(), (0, family));
    assert_eq!(keys.keys("l").line_col(), (0, family + combining));
    assert_eq!(keys.keys("l").line_col(), (0, family + combining));
    assert_eq!(keys.keys("h").line_col(), (0, family));
    assert_eq!(keys.keys("h").line_col(), (0, 0));
    assert_eq!(keys.keys("$").line_col(), (0, family + combining));
}

#[test]
fn zero_caret_and_dollar_act_on_the_line_not_the_visual_row() {
    let mut keys = Keys::lines_of(&["  indented text"]).at(0, 8);
    assert_eq!(keys.keys("0").line_col(), (0, 0));
    assert_eq!(keys.keys("^").line_col(), (0, 2));
    assert_eq!(
        keys.keys("$").line_col(),
        (0, "  indented tex".chars().count())
    );
    // A leading zero is the motion; a zero after a digit is a count.
    let mut keys = Keys::new("abcdefghij\n\nsecond").at(0, 0);
    assert_eq!(keys.keys("10l").line_col(), (0, 9));
    assert_eq!(keys.keys("0").line_col(), (0, 0));
}

#[test]
fn word_motions_use_the_editors_boundaries_and_cross_lines() {
    let mut keys = Keys::new("one two three\n\nfour").at(0, 0);
    assert_eq!(keys.keys("w").line_col(), (0, 4));
    assert_eq!(keys.keys("w").line_col(), (0, 8));
    assert_eq!(keys.keys("w").line_col(), (1, 0));
    assert_eq!(keys.keys("b").line_col(), (0, 8));
    assert_eq!(keys.keys("2b").line_col(), (0, 0));
    assert_eq!(keys.keys("e").line_col(), (0, 2));
    assert_eq!(keys.keys("2e").line_col(), (0, 12));
    // A punctuation run is its own word, as in vim's `w`. Unicode joins a full stop to
    // the letters around it, so `a.b` is one word where vim would see three.
    let mut keys = Keys::new("foo(bar)").at(0, 0);
    assert_eq!(keys.keys("w").line_col(), (0, 3));
    assert_eq!(keys.keys("w").line_col(), (0, 4));
    assert_eq!(keys.keys("w").line_col(), (0, 7));
    let mut keys = Keys::new("a.b").at(0, 0);
    assert_eq!(keys.keys("w").line_col(), (0, 2));
}

#[test]
fn word_motions_stop_on_a_blank_line_and_at_the_document_edges() {
    let mut keys = Keys::new("one\n\n<br>\n\ntwo").at(0, 0);
    assert_eq!(keys.lines(), 3);
    assert_eq!(keys.keys("w").line_col(), (1, 0));
    assert_eq!(keys.keys("w").line_col(), (2, 0));
    assert_eq!(keys.keys("w").line_col(), (2, 2));
    assert_eq!(keys.keys("9w").line_col(), (2, 2));
    assert_eq!(keys.keys("9b").line_col(), (0, 0));
    assert_eq!(keys.keys("9e").line_col(), (2, 2));
}

#[test]
fn gg_and_g_go_to_a_line_and_land_on_its_first_non_blank() {
    let mut keys = Keys::lines_of(&["first", "  second", "third"]).at(0, 0);
    assert_eq!(keys.keys("G").line_col(), (2, 0));
    assert_eq!(keys.keys("gg").line_col(), (0, 0));
    assert_eq!(keys.keys("2gg").line_col(), (1, 2));
    assert_eq!(keys.keys("2G").line_col(), (1, 2));
    assert_eq!(keys.keys("99G").line_col(), (2, 0));
}

#[test]
fn the_normal_mode_caret_is_clamped_after_every_command() {
    let mut keys = Keys::new("longer line\n\nab").at(0, 10);
    // Moving down onto a shorter line lands past its last grapheme; the clamp pulls back.
    assert_eq!(keys.keys("j").line_col(), (1, 1));
    // An empty line has nowhere to clamp to.
    let mut keys = Keys::new("ab\n\n<br>\n\ncd").at(0, 1);
    assert_eq!(keys.keys("j").line_col(), (1, 0));
    // A horizontal rule holds no text and the caret may rest on it.
    let mut keys = Keys::new("ab\n\n***").at(0, 0);
    assert_eq!(keys.keys("j").line_col(), (1, 0));
}

// ---------------------------------------------------------------- operators

#[test]
fn dw_de_and_db_delete_what_the_motion_covers() {
    let mut keys = Keys::new("one two three").at(0, 0);
    keys.keys("dw");
    assert_eq!(keys.text(), "two three");
    keys.keys("de");
    assert_eq!(keys.text(), " three");
    let mut keys = Keys::new("one two three").at(0, 8);
    keys.keys("db");
    assert_eq!(keys.text(), "one three");
}

#[test]
fn dw_on_the_last_word_of_a_line_stops_at_its_end() {
    let mut keys = Keys::new("one two\n\nthree").at(0, 4);
    keys.keys("dw");
    assert_eq!(keys.text(), "one \nthree");
    assert_eq!(keys.line_col(), (0, 3));
}

#[test]
fn d_with_the_line_motions_takes_the_right_half_of_the_line() {
    let mut keys = Keys::new("hello world").at(0, 6);
    keys.keys("d$");
    assert_eq!(keys.text(), "hello ");
    let mut keys = Keys::new("hello world").at(0, 6);
    keys.keys("d0");
    assert_eq!(keys.text(), "world");
    let mut keys = Keys::lines_of(&["  ab cd"]).at(0, 5);
    keys.keys("d^");
    assert_eq!(keys.text(), "  cd");
}

#[test]
fn counts_on_the_operator_and_the_motion_multiply() {
    let mut keys = Keys::new("a b c d e f g").at(0, 0);
    keys.keys("2d3w");
    assert_eq!(keys.text(), "g");
    let mut keys = Keys::new("abcdefgh").at(0, 0);
    keys.keys("3d2l");
    assert_eq!(keys.text(), "gh");
}

#[test]
fn dd_yy_and_cc_work_on_whole_lines_of_mixed_kinds() {
    let mut keys = Keys::new("# head\n\n- item\n\n> quote").at(1, 0);
    keys.keys("dd");
    assert_eq!(keys.markdown(), "# head\n\n> quote");
    assert_eq!(keys.line_col(), (1, 0));

    let mut keys = Keys::new("# head\n\n- item\n\n> quote").at(0, 0);
    keys.keys("2dd");
    assert_eq!(keys.markdown(), "> quote");

    let mut keys = Keys::new("- one\n- two").at(0, 0);
    keys.keys("cc");
    assert_eq!(keys.state.mode, Mode::Insert);
    assert_eq!(keys.markdown(), "- \n- two");
}

#[test]
fn dj_and_dk_are_linewise_however_the_lines_wrap() {
    let mut keys = Keys::new("one\n\ntwo\n\nthree\n\nfour").at(1, 1);
    keys.keys("dj");
    assert_eq!(keys.text(), "one\nfour");
    let mut keys = Keys::new("one\n\ntwo\n\nthree\n\nfour").at(2, 1);
    keys.keys("dk");
    assert_eq!(keys.text(), "one\nfour");
}

#[test]
fn dgg_and_dg_take_whole_lines_to_the_document_edges() {
    let mut keys = Keys::new("one\n\ntwo\n\nthree").at(1, 0);
    keys.keys("dG");
    assert_eq!(keys.text(), "one");
    let mut keys = Keys::new("one\n\ntwo\n\nthree").at(1, 0);
    keys.keys("dgg");
    assert_eq!(keys.text(), "three");
}

#[test]
fn deleting_every_line_leaves_the_smallest_valid_document() {
    let mut keys = Keys::new("# one\n\n- two\n\n***").at(0, 0);
    keys.keys("dG");
    assert_eq!(keys.text(), "");
    assert_eq!(keys.lines(), 1);
    assert_eq!(keys.line_col(), (0, 0));
    keys.keys("u");
    assert_eq!(keys.markdown(), "# one\n\n- two\n\n---");
}

#[test]
fn x_takes_graphemes_within_the_line_and_never_touches_a_rule() {
    let mut keys = Keys::new("héllo").at(0, 0);
    keys.keys("x");
    assert_eq!(keys.text(), "éllo");
    keys.keys("2x");
    assert_eq!(keys.text(), "lo");
    keys.keys("9x");
    assert_eq!(keys.text(), "");
    let mut keys = Keys::new("***\n\nafter").at(0, 0);
    keys.keys("x");
    assert_eq!(keys.markdown(), "---\n\nafter");
}

#[test]
fn shift_d_and_shift_c_clear_the_rest_of_the_line() {
    let mut keys = Keys::new("hello world").at(0, 5);
    keys.keys("D");
    assert_eq!(keys.text(), "hello");
    assert_eq!(keys.state.mode, Mode::Normal);
    let mut keys = Keys::new("# hello world").at(0, 5);
    keys.keys("C");
    assert_eq!(keys.markdown(), "# hello");
    assert_eq!(keys.state.mode, Mode::Insert);
}

/// A code block is one line, so `dd` on it takes the whole block rather than a row of
/// it. That is the model's own notion of a line and the closest thing to vim's.
#[test]
fn dd_on_a_code_block_takes_the_whole_block() {
    let mut keys = Keys::new("```rust\na\nb\n```\n\nafter").at(0, 0);
    assert_eq!(keys.lines(), 2);
    keys.keys("dd");
    assert_eq!(keys.markdown(), "after");
    keys.keys("u");
    keys.keys("yyGp");
    assert_eq!(
        keys.markdown(),
        "```rust\na\nb\n```\n\nafter\n\n```rust\na\nb\n```"
    );
}

#[test]
fn dd_on_a_nested_list_item_keeps_the_nesting_around_it() {
    let mut keys = Keys::new("- a\n  - b\n  - c\n- d").at(1, 0);
    keys.keys("dd");
    assert_eq!(keys.markdown(), "- a\n  - c\n- d");
}

// ---------------------------------------------------------------- yank and paste

#[test]
fn a_linewise_yank_pastes_below_and_above_as_whole_lines() {
    let mut keys = Keys::new("- **one**\n- two").at(0, 0);
    keys.keys("yy");
    assert_eq!(keys.line_col(), (0, 0));
    keys.keys("jp");
    assert_eq!(keys.markdown(), "- **one**\n- two\n- **one**");
    keys.keys("P");
    assert_eq!(keys.markdown(), "- **one**\n- two\n- **one**\n- **one**");
}

#[test]
fn a_charwise_yank_pastes_inline_after_and_at_the_cursor() {
    let mut keys = Keys::new("abcd").at(0, 0);
    keys.keys("ylp");
    assert_eq!(keys.text(), "aabcd");
    assert_eq!(keys.line_col(), (0, 1));
    let mut keys = Keys::new("abcd").at(0, 0);
    keys.keys("ylP");
    assert_eq!(keys.text(), "aabcd");
}

#[test]
fn a_delete_fills_the_register_so_dd_then_p_moves_a_line() {
    let mut keys = Keys::new("one\n\ntwo\n\nthree").at(0, 0);
    keys.keys("ddjp");
    assert_eq!(keys.text(), "two\nthree\none");
}

#[test]
fn a_clipboard_from_elsewhere_pastes_inline_rather_than_as_lines() {
    let mut keys = Keys::new("one\n\ntwo").at(0, 0);
    keys.keys("yy");
    // Something else took the clipboard: the linewise flag no longer applies.
    let schema = keys.host.state.schema().clone();
    keys.host.clipboard =
        Some(markraft_commonmark::from_markdown_fragment(&schema, "X").expect("a fragment"));
    keys.keys("p");
    assert_eq!(keys.text(), "oXne\ntwo");
}

#[test]
fn paste_of_a_multi_line_linewise_register_keeps_every_kind() {
    let mut keys = Keys::new("# head\n\n- [ ] task\n\nlast").at(0, 0);
    keys.keys("2yy");
    keys.keys("G");
    keys.keys("p");
    assert_eq!(
        keys.markdown(),
        "# head\n\n- [ ] task\n\nlast\n\n# head\n\n- [ ] task"
    );
}

// ---------------------------------------------------------------- insert mode

#[test]
fn the_insert_commands_place_the_caret_where_vim_does() {
    let mut keys = Keys::lines_of(&["  hello"]).at(0, 4);
    keys.keys("i");
    assert_eq!(keys.caret(), (0, 4));
    keys.keys("<esc>");
    assert_eq!(keys.line_col(), (0, 3));
    keys.keys("a");
    assert_eq!(keys.caret(), (0, 4));
    keys.keys("<esc>I");
    assert_eq!(keys.caret(), (0, 2));
    keys.keys("<esc>A");
    assert_eq!(keys.caret(), (0, 7));
}

#[test]
fn escape_from_insert_steps_left_but_not_off_the_start_of_the_line() {
    let mut keys = Keys::new("ab").at(0, 0);
    keys.keys("i").typed("XY").keys("<esc>");
    assert_eq!(keys.text(), "XYab");
    assert_eq!(keys.line_col(), (0, 1));
    let mut keys = Keys::new("ab").at(0, 0);
    keys.keys("<esc>");
    assert_eq!(keys.line_col(), (0, 0));
    // Appending leaves the caret past the last grapheme; escape still steps only one.
    let mut keys = Keys::new("ab").at(0, 0);
    keys.keys("A").typed("XY").keys("<esc>");
    assert_eq!(keys.line_col(), (0, 3));
    keys.keys("i").typed("Z").keys("<esc>");
    assert_eq!(keys.text(), "abXZY");
}

#[test]
fn o_and_shift_o_open_the_line_enter_would_make() {
    let mut keys = Keys::new("- [x] done\n\n# head").at(0, 0);
    keys.keys("o").typed("next").keys("<esc>");
    assert_eq!(keys.markdown(), "- [x] done\n- [ ] next\n\n# head");
    keys.keys("G");
    keys.keys("O").typed("above").keys("<esc>");
    assert_eq!(keys.markdown(), "- [x] done\n- [ ] next\n\nabove\n\n# head");
}

#[test]
fn o_inside_a_code_block_opens_another_row_of_it() {
    let mut keys = Keys::new("```python\nprint(1)\n```").at(0, 0);
    keys.keys("o").typed("print(2)").keys("<esc>");
    assert_eq!(keys.markdown(), "```python\nprint(1)\nprint(2)\n```");
}

// ---------------------------------------------------------------- visual modes

#[test]
fn a_visual_selection_includes_the_grapheme_under_the_cursor() {
    let mut keys = Keys::new("abcdef").at(0, 1);
    keys.keys("v");
    assert_eq!(keys.selected(), "b");
    keys.keys("ll");
    assert_eq!(keys.selected(), "bcd");
    assert_eq!(keys.line_col(), (0, 3));
    keys.keys("d");
    assert_eq!(keys.text(), "aef");
    assert_eq!(keys.state.mode, Mode::Normal);
}

#[test]
fn a_backwards_visual_selection_covers_both_ends() {
    let mut keys = Keys::new("abcdef").at(0, 4);
    keys.keys("vhh");
    assert_eq!(keys.selected(), "cde");
    assert_eq!(keys.line_col(), (0, 2));
    keys.keys("y");
    assert_eq!(keys.line_col(), (0, 2));
    keys.keys("$p");
    assert_eq!(keys.text(), "abcdefcde");
}

#[test]
fn a_visual_selection_that_crosses_its_anchor_turns_round() {
    let mut keys = Keys::new("abcdef").at(0, 2);
    keys.keys("vhh");
    assert_eq!(keys.selected(), "abc");
    keys.keys("llll");
    assert_eq!(keys.selected(), "cde");
    keys.keys("x");
    assert_eq!(keys.text(), "abf");
}

#[test]
fn visual_line_mode_takes_whole_lines_in_both_directions() {
    let mut keys = Keys::new("# one\n\ntwo\n\n- three\n\nfour").at(1, 0);
    keys.keys("V");
    assert_eq!(keys.selected(), "two");
    keys.keys("j");
    assert_eq!(keys.selected(), "two\nthree");
    keys.keys("d");
    assert_eq!(keys.markdown(), "# one\n\nfour");

    let mut keys = Keys::new("one\n\ntwo\n\nthree").at(2, 0);
    keys.keys("Vky");
    assert!(keys.state.register.as_ref().expect("a yank").linewise);
    keys.keys("Gp");
    assert_eq!(keys.text(), "one\ntwo\nthree\ntwo\nthree");
}

#[test]
fn v_and_shift_v_toggle_off_and_switch_between_each_other() {
    let mut keys = Keys::new("abc\n\ndef").at(0, 1);
    keys.keys("v");
    assert_eq!(keys.state.mode, Mode::Visual);
    keys.keys("V");
    assert_eq!(keys.state.mode, Mode::VisualLine);
    assert_eq!(keys.selected(), "abc");
    keys.keys("v");
    assert_eq!(keys.state.mode, Mode::Visual);
    keys.keys("v");
    assert_eq!(keys.state.mode, Mode::Normal);
    // Visual Line mode tracks whole lines, so leaving it lands at the line's start.
    assert_eq!(keys.line_col(), (0, 0));
}

#[test]
fn escape_leaves_a_visual_mode_without_changing_the_document() {
    let mut keys = Keys::new("abc").at(0, 0);
    keys.keys("vl<esc>");
    assert_eq!(keys.state.mode, Mode::Normal);
    assert_eq!(keys.text(), "abc");
    assert_eq!(keys.line_col(), (0, 1));
}

#[test]
fn a_visual_change_leaves_insert_mode_with_the_selection_gone() {
    let mut keys = Keys::new("one two").at(0, 0);
    keys.keys("vllc");
    assert_eq!(keys.state.mode, Mode::Insert);
    assert_eq!(keys.text(), " two");
}

#[test]
fn a_visual_selection_spans_lines() {
    let mut keys = Keys::new("abc\n\ndef").at(0, 1);
    keys.keys("vjl");
    assert_eq!(keys.selected(), "bc\ndef");
    keys.keys("d");
    assert_eq!(keys.text(), "a");
}

// ---------------------------------------------------------------- history

#[test]
fn an_insert_session_undoes_as_one_step() {
    // Typing, Return and an input rule inside one session: still one step.
    let mut keys = Keys::new("- item").at(0, 0);
    keys.keys("A")
        .typed(" one")
        .enter()
        .typed("two")
        .enter()
        .typed("# ")
        .typed("head");
    keys.keys("<esc>");
    // The heading input rule sets the *block's* type, so inside a list item it makes
    // a heading in the item rather than replacing the item.
    assert_eq!(keys.markdown(), "- item one\n- two\n- # head");
    keys.keys("u");
    assert_eq!(keys.markdown(), "- item");
    keys.keys("r");
    assert_eq!(keys.markdown(), "- item one\n- two\n- # head");

    // The edit that opened the session belongs to it: `o` and `cw` with their text.
    let mut keys = Keys::new("one two").at(0, 0);
    keys.keys("o").typed("below").keys("<esc>");
    assert_eq!(keys.text(), "one two\nbelow");
    keys.keys("u");
    assert_eq!(keys.text(), "one two");
    // Undo restores the selection from before the session, which is where `o` left
    // it; `0` puts the cursor back on the word the change is meant to take.
    keys.keys("0").keys("cw").typed("uno").keys("<esc>");
    assert_eq!(keys.text(), "uno two");
    keys.keys("u");
    assert_eq!(keys.text(), "one two");

    // Two sessions are two steps.
    keys.keys("A").typed(" three").keys("<esc>");
    keys.keys("A").typed(" four").keys("<esc>");
    keys.keys("u");
    assert_eq!(keys.text(), "one two three");
}

#[test]
fn every_normal_mode_command_is_one_undo_step() {
    let mut keys = Keys::new("one two three").at(0, 0);
    keys.keys("dw");
    keys.keys("dw");
    assert_eq!(keys.text(), "three");
    keys.keys("u");
    assert_eq!(keys.text(), "two three");
    keys.keys("u");
    assert_eq!(keys.text(), "one two three");
    keys.keys("r");
    assert_eq!(keys.text(), "two three");
    keys.keys("r");
    assert_eq!(keys.text(), "three");
}

#[test]
fn a_count_repeats_undo_and_redo() {
    let mut keys = Keys::new("abcdef").at(0, 0);
    keys.keys("xxx");
    assert_eq!(keys.text(), "def");
    keys.keys("3u");
    assert_eq!(keys.text(), "abcdef");
    keys.keys("2r");
    assert_eq!(keys.text(), "cdef");
    // Undoing past the start stops rather than looping.
    keys.keys("9u");
    assert_eq!(keys.text(), "abcdef");
}

#[test]
fn a_linewise_change_is_one_step_even_across_several_lines() {
    let mut keys = Keys::new("one\n\ntwo\n\nthree").at(0, 0);
    keys.keys("2cc");
    assert_eq!(keys.text(), "\nthree");
    keys.keys("<esc>u");
    assert_eq!(keys.text(), "one\ntwo\nthree");
}

#[test]
fn a_paste_undoes_in_one_step() {
    let mut keys = Keys::new("one\n\ntwo").at(0, 0);
    keys.keys("yyp");
    assert_eq!(keys.text(), "one\none\ntwo");
    keys.keys("u");
    assert_eq!(keys.text(), "one\ntwo");
}

// ---------------------------------------------------------------- pending state

#[test]
fn escape_forgets_a_half_typed_command_without_moving() {
    let mut keys = Keys::new("one two three").at(0, 4);
    keys.keys("2d");
    assert!(!keys.state.pending.is_empty());
    keys.keys("<esc>");
    assert!(keys.state.pending.is_empty());
    assert_eq!(keys.line_col(), (0, 4));
    assert_eq!(keys.text(), "one two three");
    // The forgotten count does not leak into the next command.
    keys.keys("x");
    assert_eq!(keys.text(), "one wo three");
}

#[test]
fn a_second_operator_replaces_the_one_waiting() {
    let mut keys = Keys::new("one two").at(0, 0);
    keys.keys("dy");
    assert_eq!(keys.state.pending.operator(), Some(Operator::Yank));
    keys.keys("w");
    assert_eq!(keys.text(), "one two");
    assert_eq!(keys.state.register.as_ref().expect("a yank").text, "one ");
}

// ---------------------------------------------------------------- unicode

#[test]
fn operators_take_whole_grapheme_clusters() {
    let family = "👩‍👩‍👧";
    let mut keys = Keys::new(&format!("{family}a")).at(0, 0);
    keys.keys("x");
    assert_eq!(keys.text(), "a");
    let mut keys = Keys::new("e\u{0301}x").at(0, 0);
    keys.keys("x");
    assert_eq!(keys.text(), "x");
    // Yanking one cluster and pasting it keeps it whole.
    let mut keys = Keys::new(&format!("{family}x")).at(0, 0);
    keys.keys("ylp");
    assert_eq!(keys.text(), format!("{family}{family}x"));
}

#[test]
fn cjk_text_moves_and_deletes_by_character() {
    let mut keys = Keys::new("中文字").at(0, 0);
    assert_eq!(keys.keys("l").line_col(), (0, 1));
    assert_eq!(keys.keys("$").line_col(), (0, 2));
    keys.keys("0");
    keys.keys("2x");
    assert_eq!(keys.text(), "字");
}

#[test]
fn a_charwise_yank_across_lines_keeps_its_marks_when_pasted_back() {
    let mut keys = Keys::new("a **bold** c\n\nsecond").at(0, 2);
    keys.keys("v$y");
    assert_eq!(keys.state.register.as_ref().expect("a yank").text, "bold c");
    keys.keys("G$p");
    assert_eq!(keys.markdown(), "a **bold** c\n\nsecond**bold** c");
}

#[test]
fn cw_leaves_insert_mode_ready_at_the_gap_it_made() {
    let mut keys = Keys::new("one two").at(0, 0);
    keys.keys("cw");
    assert_eq!(keys.state.mode, Mode::Insert);
    // Like vim, `cw` on a word acts as `ce`: the space after it stays.
    assert_eq!(keys.text(), " two");
    keys.typed("ONE");
    assert_eq!(keys.text(), "ONE two");
    keys.keys("<esc>");
    assert_eq!(keys.state.mode, Mode::Normal);
}

#[test]
fn dd_on_the_only_line_leaves_an_empty_document_and_undoes() {
    let mut keys = Keys::new("# only").at(0, 0);
    keys.keys("dd");
    assert_eq!(keys.text(), "");
    assert_eq!(keys.lines(), 1);
    keys.keys("p");
    // Paste after an emptied document leaves a blank separator before the heading.
    assert_eq!(keys.markdown(), "\n# only");
    keys.keys("u");
    assert_eq!(keys.text(), "");
    keys.keys("u");
    assert_eq!(keys.markdown(), "# only");
}

#[test]
fn a_paste_into_a_code_block_stays_literal() {
    let mut keys = Keys::new("**bold**\n\n```rust\nx\n```").at(0, 0);
    keys.keys("v$y");
    keys.keys("j$p");
    assert_eq!(keys.markdown(), "**bold**\n\n```rust\nxbold\n```");
}

/// The block caret is painted from the editor's own selection, so leaving
/// Insert at a line end has to put *that* on the last grapheme, not only vim's
/// notion of the cursor.
#[test]
fn escape_at_a_line_end_leaves_the_caret_on_the_last_grapheme() {
    let mut keys = Keys::new("end").at(0, 0);
    keys.keys("A").typed("hi").keys("<esc>");
    assert_eq!(keys.text(), "endhi");
    assert_eq!(keys.line_col(), (0, 4));
    assert_eq!(
        keys.caret(),
        (0, 4),
        "the editor's caret, which the block covers"
    );
    // The same by way of `o`, which is how a session usually starts.
    let mut keys = Keys::new("end").at(0, 0);
    keys.keys("o").typed("hi").keys("<esc>");
    assert_eq!(keys.text(), "end\nhi");
    assert_eq!(keys.line_col(), (1, 1));
    assert_eq!(keys.caret(), (1, 1));
    // And on a line the session left empty, where there is no grapheme to rest
    // on at all.
    let mut keys = Keys::new("end").at(0, 0);
    keys.keys("o").keys("<esc>");
    assert_eq!(keys.line_col(), (1, 0));
    assert_eq!(keys.caret(), (1, 0));
}

// ---------------------------------------------------------------- input method

/// The live repro: `o`, a composed candidate the input method commits, then
/// Escape. The caret has to end up on the committed character, inside the line.
#[test]
fn escape_after_an_input_method_commit_lands_on_the_committed_character() {
    let mut keys = Keys::new("end").at(0, 0);
    keys.keys("o").composed("h").keys("<esc>");
    assert_eq!(keys.text(), "end\nh");
    assert_eq!(keys.state.mode, Mode::Normal);
    assert_eq!(keys.line_col(), (1, 0));
    assert_eq!(keys.caret(), (1, 0), "the caret the block is painted from");
    let head = host::head(&keys.host);
    let projection = keys.host.projection();
    assert!(
        projection.is_caret_position(head),
        "the caret sits in a textblock"
    );
    // And it moves from there the way Normal mode expects.
    assert_eq!(keys.keys("k").line_col(), (0, 0));
    assert_eq!(keys.keys("j").line_col(), (1, 0));
}

/// The same, with more than one character, so the step back is visible.
#[test]
fn escape_after_a_longer_commit_steps_back_one_grapheme() {
    let mut keys = Keys::new("end").at(0, 0);
    keys.keys("o").composed("hi").keys("<esc>");
    assert_eq!(keys.text(), "end\nhi");
    assert_eq!(keys.line_col(), (1, 1));
    assert_eq!(keys.caret(), (1, 1));
}

/// A transaction vim did not make — an undo restoring a selection, an
/// extension's edit, a click — may leave the caret past the last grapheme.
/// Normal mode never rests there, so settling has to pull it back.
#[test]
fn an_external_selection_at_a_line_end_is_settled_onto_the_last_grapheme() {
    let mut keys = Keys::new("end").at(0, 0);
    let end = keys.host.projection().lines()[0].to;
    keys.external(vec![
        TransactionSpec::new().selection(Selection::cursor(end)),
    ]);
    assert_eq!(keys.line_col(), (0, 2));
    assert_eq!(keys.caret(), (0, 2), "the caret the block is painted from");
}

/// Undo restores the selection from before the entry, which may sit at a line
/// end; Normal mode settles it the same way.
#[test]
fn the_caret_after_an_undo_rests_on_a_grapheme() {
    let mut keys = Keys::new("end").at(0, 0);
    keys.keys("A").typed("hi").keys("<esc>");
    assert_eq!(keys.text(), "endhi");
    keys.keys("u");
    assert_eq!(keys.text(), "end");
    assert_eq!(keys.line_col(), (0, 2));
    assert_eq!(keys.caret(), (0, 2));
}

/// A transaction from outside vim may leave a selection that is not a text
/// cursor at all — `Selection::near` answers a node selection wherever the
/// position it was given is not inline content. Normal mode has no caret there,
/// so settling has to collapse it onto a grapheme.
#[test]
fn an_external_node_selection_is_settled_into_the_text() {
    let mut keys = Keys::new("one\n\n***\n\ntwo").at(0, 0);
    let rule = keys.host.projection().lines()[1]
        .ancestors
        .last()
        .expect("the rule's own node")
        .before;
    keys.external(vec![
        TransactionSpec::new().selection(Selection::node(rule)),
    ]);
    let head = host::head(&keys.host);
    let projection = keys.host.projection();
    assert!(
        projection.line_at(head).is_some(),
        "the caret sits in a line, not on a boundary between blocks"
    );
    assert!(keys.host.state().selection().is_cursor());
    // A rule holds no text, so resting on it is what vim intends there; the
    // caret is still somewhere the view can paint and a motion can start from.
    assert_eq!(keys.line_col(), (1, 0));
    assert_eq!(keys.keys("k").line_col(), (0, 0));
}

#[test]
fn linewise_operators_preserve_unselected_container_children() {
    for sequence in ["2dd", "Vjd"] {
        let mut keys = Keys::new("one\n\n- two\n- three").at(0, 0);
        keys.keys(sequence);
        assert_eq!(keys.markdown(), "- three", "{sequence}");
        keys.keys("u");
        assert_eq!(keys.markdown(), "one\n\n- two\n- three");
    }
    let mut keys = Keys::new("one\n\n- two\n- three").at(0, 0);
    keys.keys("2cc").typed("new").keys("<esc>");
    assert_eq!(keys.markdown(), "new\n\n- three");
    let mut keys = Keys::new("one\n\n- two\n- three").at(0, 0);
    keys.keys("2yyGp");
    assert_eq!(keys.markdown(), "one\n\n- two\n- three\n\none\n\n- two");
    let mut keys = Keys::new("- one\n- two\n\nthree").at(1, 0);
    keys.keys("2dd");
    assert_eq!(keys.markdown(), "- one");
    let mut keys = Keys::new("- one\n  - two\n  - three\n- four").at(0, 0);
    keys.keys("2dd");
    assert_eq!(keys.text(), "three\nfour");
    let mut keys = Keys::new("> one\n>\n> two\n\nthree").at(1, 0);
    keys.keys("2dd");
    assert_eq!(keys.markdown(), "> one");
}

#[test]
fn linewise_paste_next_to_a_leaf_block_has_no_ancestor_requirement() {
    let mut keys = Keys::new("text\n\n***").at(0, 0);
    keys.keys("yyjp");
    assert_eq!(keys.markdown(), "text\n\n---\n\ntext");
    let mut keys = Keys::new("text\n\n***").at(0, 0);
    keys.keys("yyjP");
    assert_eq!(keys.markdown(), "text\n\ntext\n\n---");
}

#[test]
fn shift_o_in_code_keeps_the_cursor_in_the_new_code_row() {
    let mut keys = Keys::new("before\n\n```\ncode\n```").at(1, 0);
    keys.keys("O").typed("new").keys("<esc>");
    assert_eq!(keys.markdown(), "before\n\n```\nnew\ncode\n```");
    keys.keys("u");
    assert_eq!(keys.markdown(), "before\n\n```\ncode\n```");
}

#[test]
fn inline_containers_do_not_add_vim_caret_stops() {
    let mut keys = Keys::new("**a **b**** c").at(0, 0);
    let width = keys
        .host
        .projection()
        .line_text(0)
        .expect("a line")
        .chars()
        .count();
    for column in 1..width {
        keys.keys("l");
        assert_eq!(keys.line_col(), (0, column));
        let projection = keys.host.projection();
        assert!(projection.is_caret_position(host::head(&keys.host)));
    }
    for column in (0..width - 1).rev() {
        keys.keys("h");
        assert_eq!(keys.line_col(), (0, column));
    }
    keys.keys("$0");
    assert_eq!(keys.line_col(), (0, 0));
    // The prose is what those delimiters spell, whatever they cost to walk.
    assert_eq!(
        markraft_commonmark::to_plain_text(keys.host.state.schema(), keys.host.state.doc()),
        "a b c"
    );
}

#[test]
fn charwise_yanks_preserve_nested_inline_scopes_when_pasted_into_plain_text() {
    /// Nesting the schema cannot keep flat lives in an inline container, which
    /// carries its own share of the marks.
    fn marks_on_word(
        schema: &markraft_core::Schema,
        content: &markraft_core::Fragment,
        inherited: markraft_core::MarkSet,
    ) -> Option<markraft_core::MarkSet> {
        content.iter().find_map(|node| {
            let marks = node
                .marks()
                .iter()
                .fold(inherited.clone(), |set, mark| set.add(schema, mark.clone()));
            if node.text() == Some("word") {
                Some(marks)
            } else if node.is_container() {
                marks_on_word(schema, node.content(), marks)
            } else {
                None
            }
        })
    }

    for (source, outer) in [("*a **word** c*", "em"), ("**a **word** c**", "strong")] {
        for yank in ["yw", "v3ly"] {
            let keys = Keys::new(&format!("{source}\n\nx")).at(0, 0);
            // Method-B puts the delimiter characters in the line, so the word
            // starts wherever they leave it.
            let column = keys
                .host
                .projection()
                .line_text(0)
                .expect("a line")
                .find("word")
                .expect("the word is in the line");
            let mut keys = keys.at(0, column);
            keys.keys(yank);
            let schema = keys.host.state.schema().clone();
            let slice = keys.host.clipboard.as_ref().expect("a copied word");
            // A slice holds a mark *set*, not the order the source nested them
            // in, so what is checked is the marks rather than a spelling of them.
            let marks = marks_on_word(&schema, slice.content(), markraft_core::MarkSet::empty())
                .unwrap_or_else(|| panic!("{source} {yank}: no word in {slice:?}"));
            for name in ["strong", outer] {
                let ty = schema.mark_id(name).expect("the mark type");
                assert!(
                    marks.contains_type(ty),
                    "{source} {yank}: the word lost {name}"
                );
            }
            keys.keys("G$p");
            let plain = markraft_commonmark::to_plain_text(&schema, keys.host.state.doc());
            assert_eq!(
                plain.lines().last().expect("a last line"),
                "xword",
                "{source} {yank}"
            );
        }
    }
}

// ---------------------------------------------------------------- tables

/// A paragraph, a table of a header row and one body row, and a paragraph. Every
/// cell is a line, in row-major order, so the lines are `before`, the three header
/// cells, the three body cells and `after`.
const TABLE: &str = "before\n\n| a | b | c |\n| --- | --- | --- |\n| d | e | f |\n\nafter";

/// The same table with room to move inside a cell.
const WIDE: &str = "| one two | b |\n| --- | --- |\n| three | d |";

#[test]
fn dd_takes_the_row_and_the_last_row_takes_the_table() {
    let mut keys = Keys::new(TABLE).at(4, 0);
    keys.keys("dd");
    assert_eq!(
        keys.markdown(),
        "before\n\n| a   | b   | c   |\n| --- | --- | --- |\n\nafter"
    );
    // The caret stays in the grid, in the column it was in and in the row that took
    // the deleted one's place — here the header, the deleted row being the last.
    assert_eq!(keys.line_col(), (1, 0));
    // Deleting the only row deletes the table, which may not be empty.
    keys.keys("dd");
    assert_eq!(keys.markdown(), "before\n\nafter");
    // Each is one undo step.
    assert_eq!(
        keys.keys("u").markdown(),
        "before\n\n| a   | b   | c   |\n| --- | --- | --- |\n\nafter"
    );
    assert_eq!(keys.keys("u").markdown(), Keys::new(TABLE).markdown());
}

#[test]
fn dd_on_the_header_makes_the_row_below_it_the_header() {
    let mut keys = Keys::new(TABLE).at(2, 0);
    keys.keys("dd");
    assert_eq!(
        keys.markdown(),
        "before\n\n| d   | e   | f   |\n| --- | --- | --- |\n\nafter"
    );
}

#[test]
fn a_count_and_dj_count_rows_not_cells() {
    let mut keys = Keys::new(TABLE).at(1, 0);
    keys.keys("dj");
    assert_eq!(keys.markdown(), "before\n\nafter");
    let mut keys = Keys::new(TABLE).at(2, 0);
    keys.keys("2dd");
    assert_eq!(keys.markdown(), "before\n\nafter");
    // A count that runs past the last row stops at the end of the grid.
    let mut keys = Keys::new(TABLE).at(5, 0);
    keys.keys("9dd");
    assert_eq!(
        keys.markdown(),
        "before\n\n| a   | b   | c   |\n| --- | --- | --- |\n\nafter"
    );
}

#[test]
fn yy_takes_the_whole_row_and_p_puts_a_row_below_or_above() {
    let mut keys = Keys::new(TABLE).at(4, 0);
    keys.keys("yy");
    let register = keys.state.register.clone().expect("a yanked row");
    assert!(register.linewise);
    assert_eq!(register.text, "d\ne\nf");
    // One whole `table_row` node, not the three cells and not the table.
    assert_eq!(register.slice.content().child_count(), 1);
    assert_eq!(
        keys.host
            .state
            .schema()
            .node_type(register.slice.content().child(0).type_id())
            .name(),
        "table_row"
    );
    // Below and above the cursor's row, not its cell.
    let mut keys = Keys::new(TABLE).at(1, 0);
    keys.keys("yy");
    let pos = keys.pos(5, 0);
    keys.host.select(Selection::cursor(pos), false);
    keys.settle();
    keys.keys("p");
    assert_eq!(
        keys.markdown(),
        "before\n\n| a   | b   | c   |\n| --- | --- | --- |\n| d   | e   | f   |\n| a   | b   | c   |\n\nafter"
    );
    assert_eq!(keys.keys("u").markdown(), Keys::new(TABLE).markdown());
    keys.keys("P");
    assert_eq!(
        keys.markdown(),
        "before\n\n| a   | b   | c   |\n| --- | --- | --- |\n| a   | b   | c   |\n| d   | e   | f   |\n\nafter"
    );
}

#[test]
fn a_row_pasted_outside_a_table_becomes_a_table_of_its_own() {
    let mut keys = Keys::new(TABLE).at(4, 0);
    keys.keys("yy");
    keys.keys("G");
    keys.keys("p");
    assert_eq!(
        keys.markdown(),
        "before\n\n| a   | b   | c   |\n| --- | --- | --- |\n| d   | e   | f   |\n\nafter\n\n| d   | e   | f   |\n| --- | --- | --- |"
    );
}

#[test]
fn cc_clears_the_cell_and_leaves_the_row_whole() {
    let mut keys = Keys::new(TABLE).at(4, 0);
    keys.keys("cc");
    assert_eq!(keys.state.mode, Mode::Insert);
    keys.typed("x");
    keys.keys("<esc>");
    assert_eq!(
        keys.markdown(),
        "before\n\n| a   | b   | c   |\n| --- | --- | --- |\n| x   | e   | f   |\n\nafter"
    );
}

#[test]
fn o_and_shift_o_add_a_row_and_open_insert_in_the_same_column() {
    let mut keys = Keys::new(TABLE).at(5, 0);
    keys.keys("o");
    assert_eq!(keys.state.mode, Mode::Insert);
    keys.typed("x");
    keys.keys("<esc>");
    assert_eq!(
        keys.markdown(),
        "before\n\n| a   | b   | c   |\n| --- | --- | --- |\n| d   | e   | f   |\n|     | x   |     |\n\nafter"
    );
    assert_eq!(keys.keys("u").markdown(), Keys::new(TABLE).markdown());
    let mut keys = Keys::new(TABLE).at(5, 0);
    keys.keys("O");
    keys.typed("x");
    keys.keys("<esc>");
    assert_eq!(
        keys.markdown(),
        "before\n\n| a   | b   | c   |\n| --- | --- | --- |\n|     | x   |     |\n| d   | e   | f   |\n\nafter"
    );
}

#[test]
fn j_and_k_step_rows_and_leave_the_table_at_its_edges() {
    let mut keys = Keys::new(TABLE).at(2, 0);
    assert_eq!(keys.keys("j").line_col(), (5, 0));
    // No row is appended at the last one; `j` leaves the table.
    assert_eq!(keys.keys("j").line_col(), (7, 0));
    assert_eq!(keys.lines(), 8);
    let mut keys = Keys::new(TABLE).at(5, 0);
    assert_eq!(keys.keys("k").line_col(), (2, 0));
    assert_eq!(keys.keys("k").line_col(), (0, 0));
}

#[test]
fn h_and_l_cross_into_the_cell_beside_and_stop_at_the_table() {
    let mut keys = Keys::new(TABLE).at(1, 0);
    assert_eq!(keys.keys("l").line_col(), (2, 0));
    assert_eq!(keys.keys("h").line_col(), (1, 0));
    // The first cell of the table has nothing before it.
    assert_eq!(keys.keys("h").line_col(), (1, 0));
    // Nor does the last have anything after it: no row is appended.
    let mut keys = Keys::new(TABLE).at(6, 0);
    assert_eq!(keys.keys("l").line_col(), (6, 0));
    assert_eq!(keys.lines(), 8);
    // Inside a cell they stay in it.
    let mut keys = Keys::new(WIDE).at(0, 0);
    assert_eq!(keys.keys("l").line_col(), (0, 1));
    // `dl` takes the grapheme it is on rather than crossing.
    let mut keys = Keys::new(TABLE).at(1, 0);
    keys.keys("dl");
    assert_eq!(
        keys.markdown(),
        "before\n\n|     | b   | c   |\n| --- | --- | --- |\n| d   | e   | f   |\n\nafter"
    );
}

#[test]
fn zero_caret_and_dollar_act_on_the_cell() {
    let mut keys = Keys::new(WIDE).at(0, 4);
    assert_eq!(keys.keys("0").line_col(), (0, 0));
    assert_eq!(keys.keys("$").line_col(), (0, 6));
    assert_eq!(keys.keys("^").line_col(), (0, 0));
}

#[test]
fn word_motions_cross_cells_the_way_they_cross_lines() {
    let mut keys = Keys::new(WIDE).at(0, 0);
    assert_eq!(keys.keys("w").line_col(), (0, 4));
    assert_eq!(keys.keys("w").line_col(), (1, 0));
    assert_eq!(keys.keys("b").line_col(), (0, 4));
    assert_eq!(keys.keys("e").line_col(), (0, 6));
    assert_eq!(keys.keys("e").line_col(), (1, 0));
}

#[test]
fn gg_and_g_keep_counting_lines_through_a_table() {
    let mut keys = Keys::new(TABLE).at(0, 0);
    assert_eq!(keys.keys("G").line_col(), (7, 0));
    assert_eq!(keys.keys("3gg").line_col(), (2, 0));
    assert_eq!(keys.keys("gg").line_col(), (0, 0));
}

#[test]
fn visual_line_mode_takes_the_whole_row() {
    let mut keys = Keys::new(TABLE).at(4, 0);
    keys.keys("V");
    assert_eq!(keys.selected(), "d\ne\nf");
    keys.keys("d");
    assert_eq!(
        keys.markdown(),
        "before\n\n| a   | b   | c   |\n| --- | --- | --- |\n\nafter"
    );
    // Extending it downwards out of the table keeps whole rows.
    let mut keys = Keys::new(TABLE).at(1, 0);
    keys.keys("Vj");
    assert_eq!(keys.selected(), "a\nb\nc\nd\ne\nf");
}

#[test]
fn a_charwise_selection_across_two_cells_refuses_to_be_edited() {
    for operator in ["d", "c", "x"] {
        let mut keys = Keys::new(TABLE).at(1, 0);
        keys.keys("vl");
        assert_eq!(keys.selected(), "a\nb");
        keys.keys(operator);
        assert_eq!(keys.markdown(), Keys::new(TABLE).markdown(), "{operator}");
        assert_ne!(keys.state.mode, Mode::Insert, "{operator}");
    }
    // A yank changes nothing, so it is allowed to span them.
    let mut keys = Keys::new(TABLE).at(1, 0);
    keys.keys("vly");
    assert_eq!(keys.markdown(), Keys::new(TABLE).markdown());
}

#[test]
fn the_insert_commands_place_the_caret_inside_the_cell() {
    assert_eq!(Keys::new(WIDE).at(0, 4).keys("i").caret(), (0, 4));
    assert_eq!(Keys::new(WIDE).at(0, 4).keys("a").caret(), (0, 5));
    assert_eq!(Keys::new(WIDE).at(0, 4).keys("I").caret(), (0, 0));
    let mut keys = Keys::new(WIDE).at(0, 4);
    assert_eq!(keys.keys("A").caret(), (0, 7));
    keys.typed("!");
    keys.keys("<esc>");
    assert_eq!(keys.host.projection().line_text(0), Some("one two!"));
}

#[test]
fn a_linewise_operator_that_reaches_out_of_a_table_still_takes_whole_rows() {
    // `dG` from the body row takes it and everything after it.
    let mut keys = Keys::new(TABLE).at(4, 0);
    keys.keys("dG");
    assert_eq!(
        keys.markdown(),
        "before\n\n| a   | b   | c   |\n| --- | --- | --- |"
    );
    // A Visual Line selection grown over both rows takes the table with them.
    let mut keys = Keys::new(TABLE).at(2, 0);
    keys.keys("Vjd");
    assert_eq!(keys.markdown(), "before\n\nafter");
    // And a range starting outside one takes the row it reaches into whole.
    let mut keys = Keys::new(TABLE).at(0, 0);
    keys.keys("dj");
    assert_eq!(
        keys.markdown(),
        "| d   | e   | f   |\n| --- | --- | --- |\n\nafter"
    );
}

#[test]
fn a_table_that_is_the_whole_document_leaves_the_smallest_one_the_schema_allows() {
    let mut keys = Keys::new("| a | b |\n| --- | --- |").at(0, 0);
    keys.keys("dd");
    assert_eq!(keys.markdown(), "");
    assert_eq!(keys.lines(), 1);
    assert_eq!(keys.keys("u").markdown(), "| a   | b   |\n| --- | --- |");
}

#[test]
fn a_paste_that_is_not_a_row_lands_beside_the_table_rather_than_in_it() {
    let mut keys = Keys::new(TABLE).at(0, 0);
    keys.keys("yy");
    let pos = keys.pos(4, 0);
    keys.host.select(Selection::cursor(pos), false);
    keys.settle();
    keys.keys("p");
    assert_eq!(
        keys.markdown(),
        "before\n\n| a   | b   | c   |\n| --- | --- | --- |\n| d   | e   | f   |\n\nbefore\n\nafter"
    );
}

#[test]
fn x_and_shift_d_keep_to_the_cell_they_are_in() {
    let mut keys = Keys::new(TABLE).at(1, 0);
    keys.keys("x");
    assert_eq!(
        keys.markdown(),
        "before\n\n|     | b   | c   |\n| --- | --- | --- |\n| d   | e   | f   |\n\nafter"
    );
    // An empty cell is still a cell, and `h` crosses out of it.
    assert_eq!(keys.keys("l").line_col(), (2, 0));
    let mut keys = Keys::new(WIDE).at(0, 4);
    keys.keys("D");
    assert_eq!(
        keys.markdown(),
        "| one   | b   |\n| ----- | --- |\n| three | d   |"
    );
}
