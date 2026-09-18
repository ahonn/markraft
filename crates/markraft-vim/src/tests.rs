//! The whole of vim driven over a bare `markraft_core::Editor`.
//!
//! [`Keys`] implements the same [`Host`] the GPUI layer implements and dispatches a
//! keystroke to the same `command` functions the action handlers call, so these tests
//! exercise the real command layer rather than a copy of it. Only two inputs cannot be
//! reproduced without a window: `j` and `k` outside a linewise context move by visual
//! row, which here is one block, and a repaint.

use crate::{
    command::{self, InsertAt},
    host::Host,
    motion::Motion,
    state::{Mode, Operator, State},
};
use markraft_core::{
    Document, Editor, Origin, Position, Selection, Transaction, TransactionOptions,
};

/// An editor plus a clipboard, standing in for `EditorCx`.
struct Editing {
    editor: Editor,
    clipboard: Option<Document>,
}

impl Host for Editing {
    fn document(&self) -> &Document {
        self.editor.document()
    }
    fn selection(&self) -> Selection {
        self.editor.selection()
    }
    fn select(&mut self, selection: Selection, _: bool) {
        self.editor.set_selection(selection);
    }
    fn edit(&mut self, action: &mut dyn FnMut(&mut Transaction<'_>)) {
        self.editor.transact(
            TransactionOptions {
                group: None,
                origin: Origin::Extension(crate::VIM),
            },
            |tx| action(tx),
        );
    }
    /// No layout, so a visual row is a block; the column is kept as a byte offset, which
    /// is what an unwrapped line would do anyway.
    fn rows(&mut self, rows: isize, extend: bool) {
        let selection = self.editor.selection();
        let head = selection.head;
        let blocks = self.editor.document().blocks.len() as isize;
        let block = (head.block as isize)
            .saturating_add(rows)
            .clamp(0, blocks - 1) as usize;
        let head = Position {
            block,
            byte: head.byte,
        };
        self.editor.set_selection(Selection {
            anchor: if extend { selection.anchor } else { head },
            head,
        });
    }
    fn write_clipboard(&mut self, fragment: Document, _: String) {
        self.clipboard = Some(fragment);
    }
    fn read_clipboard(&mut self) -> Option<Document> {
        self.clipboard.clone()
    }
    fn begin_undo_group(&mut self) {
        self.editor.begin_undo_group();
    }
    fn end_undo_group(&mut self) {
        self.editor.end_undo_group();
    }
    fn history(&mut self, undo: bool) -> bool {
        if undo {
            self.editor.undo()
        } else {
            self.editor.redo()
        }
        .is_some()
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
        Self {
            host: Editing {
                editor: Editor::new(Document::from_markdown(markdown)),
                clipboard: None,
            },
            state: State::default(),
        }
    }

    fn at(mut self, block: usize, byte: usize) -> Self {
        self.host
            .editor
            .set_selection(Selection::caret(Position { block, byte }));
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
                let block = self.state.pending.count().map_or(0, |count| count - 1);
                command::motion(&mut self.state, &mut self.host, Motion::Block(block));
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
                let last = host.document().blocks.len() - 1;
                let block = state.pending.count().map_or(last, |count| count - 1);
                motion(state, host, Motion::Block(block));
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
    }

    /// Text typed in Insert mode, exactly as the platform delivers it.
    fn typed(&mut self, text: &str) -> &mut Self {
        assert_eq!(self.state.mode, Mode::Insert, "typing outside Insert mode");
        self.host.editor.insert_text(text);
        self
    }

    fn cursor(&self) -> Position {
        command::cursor(&self.state, &self.host)
    }

    fn text(&self) -> String {
        self.host.editor.document().plain_text()
    }

    fn markdown(&self) -> String {
        self.host.editor.document().to_markdown()
    }

    fn selected(&self) -> String {
        let (start, end) = self.host.editor.selection().ordered();
        self.host.editor.text_in(start..end)
    }
}

fn at(block: usize, byte: usize) -> Position {
    Position { block, byte }
}

// ---------------------------------------------------------------- motions

#[test]
fn h_and_l_stay_inside_the_line_and_stop_on_its_last_grapheme() {
    let mut keys = Keys::new("abc\ndef").at(0, 1);
    assert_eq!(keys.keys("h").cursor(), at(0, 0));
    assert_eq!(keys.keys("h").cursor(), at(0, 0));
    assert_eq!(keys.keys("l").cursor(), at(0, 1));
    // Normal mode never rests past the last grapheme, so `l` stops there.
    assert_eq!(keys.keys("lll").cursor(), at(0, 2));
    assert_eq!(keys.keys("9l").cursor(), at(0, 2));
    assert_eq!(keys.keys("9h").cursor(), at(0, 0));
}

#[test]
fn motions_step_whole_grapheme_clusters() {
    // A family emoji, a combining acute, and CJK.
    let mut keys = Keys::new("👩‍👩‍👧e\u{0301}中").at(0, 0);
    let family = "👩‍👩‍👧".len();
    let combining = "e\u{0301}".len();
    assert_eq!(keys.keys("l").cursor(), at(0, family));
    assert_eq!(keys.keys("l").cursor(), at(0, family + combining));
    assert_eq!(keys.keys("l").cursor(), at(0, family + combining));
    assert_eq!(keys.keys("h").cursor(), at(0, family));
    assert_eq!(keys.keys("h").cursor(), at(0, 0));
    assert_eq!(keys.keys("$").cursor(), at(0, family + combining));
}

#[test]
fn zero_caret_and_dollar_act_on_the_block_not_the_visual_row() {
    let mut keys = Keys::new("  indented text").at(0, 8);
    assert_eq!(keys.keys("0").cursor(), at(0, 0));
    assert_eq!(keys.keys("^").cursor(), at(0, 2));
    assert_eq!(keys.keys("$").cursor(), at(0, "  indented tex".len()));
    // A leading zero is the motion; a zero after a digit is a count.
    let mut keys = Keys::new("abcdefghij\nsecond").at(0, 0);
    assert_eq!(keys.keys("10l").cursor(), at(0, 9));
    assert_eq!(keys.keys("0").cursor(), at(0, 0));
}

#[test]
fn word_motions_use_the_editors_boundaries_and_cross_lines() {
    let mut keys = Keys::new("one two three\nfour").at(0, 0);
    assert_eq!(keys.keys("w").cursor(), at(0, 4));
    assert_eq!(keys.keys("w").cursor(), at(0, 8));
    assert_eq!(keys.keys("w").cursor(), at(1, 0));
    assert_eq!(keys.keys("b").cursor(), at(0, 8));
    assert_eq!(keys.keys("2b").cursor(), at(0, 0));
    assert_eq!(keys.keys("e").cursor(), at(0, 2));
    assert_eq!(keys.keys("2e").cursor(), at(0, 12));
    // A punctuation run is its own word, as in vim's `w`. Unicode joins a full stop to
    // the letters around it, so `a.b` is one word where vim would see three.
    let mut keys = Keys::new("foo(bar)").at(0, 0);
    assert_eq!(keys.keys("w").cursor(), at(0, 3));
    assert_eq!(keys.keys("w").cursor(), at(0, 4));
    assert_eq!(keys.keys("w").cursor(), at(0, 7));
    let mut keys = Keys::new("a.b").at(0, 0);
    assert_eq!(keys.keys("w").cursor(), at(0, 2));
}

#[test]
fn word_motions_stop_on_a_blank_line_and_at_the_document_edges() {
    let mut keys = Keys::new("one\n\ntwo").at(0, 0);
    assert_eq!(keys.keys("w").cursor(), at(1, 0));
    assert_eq!(keys.keys("w").cursor(), at(2, 0));
    assert_eq!(keys.keys("w").cursor(), at(2, 2));
    assert_eq!(keys.keys("9w").cursor(), at(2, 2));
    assert_eq!(keys.keys("9b").cursor(), at(0, 0));
    assert_eq!(keys.keys("9e").cursor(), at(2, 2));
}

#[test]
fn gg_and_g_go_to_a_block_and_land_on_its_first_non_blank() {
    let mut keys = Keys::new("first\n  second\nthird").at(0, 0);
    assert_eq!(keys.keys("G").cursor(), at(2, 0));
    assert_eq!(keys.keys("gg").cursor(), at(0, 0));
    assert_eq!(keys.keys("2gg").cursor(), at(1, 2));
    assert_eq!(keys.keys("2G").cursor(), at(1, 2));
    assert_eq!(keys.keys("99G").cursor(), at(2, 0));
}

#[test]
fn the_normal_mode_caret_is_clamped_after_every_command() {
    let mut keys = Keys::new("longer line\nab").at(0, 10);
    // Moving down onto a shorter line lands past its last grapheme; the clamp pulls back.
    assert_eq!(keys.keys("j").cursor(), at(1, 1));
    // An empty line has nowhere to clamp to.
    let mut keys = Keys::new("ab\n\ncd").at(0, 1);
    assert_eq!(keys.keys("j").cursor(), at(1, 0));
    // A divider holds no text and the caret may rest on it.
    let mut keys = Keys::new("ab\n***").at(0, 0);
    assert_eq!(keys.keys("j").cursor(), at(1, 0));
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
    let mut keys = Keys::new("one two\nthree").at(0, 4);
    keys.keys("dw");
    assert_eq!(keys.text(), "one \nthree");
    assert_eq!(keys.cursor(), at(0, 3));
}

#[test]
fn d_with_the_line_motions_takes_the_right_half_of_the_line() {
    let mut keys = Keys::new("hello world").at(0, 6);
    keys.keys("d$");
    assert_eq!(keys.text(), "hello ");
    let mut keys = Keys::new("hello world").at(0, 6);
    keys.keys("d0");
    assert_eq!(keys.text(), "world");
    let mut keys = Keys::new("  ab cd").at(0, 5);
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
fn dd_yy_and_cc_work_on_whole_blocks_of_mixed_kinds() {
    let mut keys = Keys::new("# head\n- item\n> quote").at(1, 0);
    keys.keys("dd");
    assert_eq!(keys.markdown(), "# head\n> quote");
    assert_eq!(keys.cursor(), at(1, 0));

    let mut keys = Keys::new("# head\n- item\n> quote").at(0, 0);
    keys.keys("2dd");
    assert_eq!(keys.markdown(), "> quote");

    let mut keys = Keys::new("- one\n- two").at(0, 0);
    keys.keys("cc");
    assert_eq!(keys.state.mode, Mode::Insert);
    assert_eq!(keys.markdown(), "- \n- two");
}

#[test]
fn dj_and_dk_are_linewise_however_the_lines_wrap() {
    let mut keys = Keys::new("one\ntwo\nthree\nfour").at(1, 1);
    keys.keys("dj");
    assert_eq!(keys.text(), "one\nfour");
    let mut keys = Keys::new("one\ntwo\nthree\nfour").at(2, 1);
    keys.keys("dk");
    assert_eq!(keys.text(), "one\nfour");
}

#[test]
fn dgg_and_dg_take_whole_blocks_to_the_document_edges() {
    let mut keys = Keys::new("one\ntwo\nthree").at(1, 0);
    keys.keys("dG");
    assert_eq!(keys.text(), "one");
    let mut keys = Keys::new("one\ntwo\nthree").at(1, 0);
    keys.keys("dgg");
    assert_eq!(keys.text(), "three");
}

#[test]
fn deleting_every_block_leaves_one_empty_paragraph() {
    let mut keys = Keys::new("# one\n- two\n***").at(0, 0);
    keys.keys("dG");
    assert_eq!(keys.host.editor.document(), &Document::default());
    assert_eq!(keys.cursor(), at(0, 0));
    keys.keys("u");
    assert_eq!(keys.markdown(), "# one\n- two\n---");
}

#[test]
fn x_takes_graphemes_within_the_line_and_never_touches_a_divider() {
    let mut keys = Keys::new("héllo").at(0, 0);
    keys.keys("x");
    assert_eq!(keys.text(), "éllo");
    keys.keys("2x");
    assert_eq!(keys.text(), "lo");
    keys.keys("9x");
    assert_eq!(keys.text(), "");
    let mut keys = Keys::new("***\nafter").at(0, 0);
    keys.keys("x");
    assert_eq!(keys.markdown(), "---\nafter");
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

#[test]
fn dd_inside_a_code_run_removes_one_code_line_and_keeps_the_run() {
    let mut keys = Keys::new("```rust\na\nb\nc\n```").at(1, 0);
    keys.keys("dd");
    assert_eq!(keys.markdown(), "```rust\na\nc\n```");
    keys.keys("yyp");
    assert_eq!(keys.markdown(), "```rust\na\nc\nc\n```");
}

#[test]
fn dd_on_a_nested_list_keeps_the_depths_around_it() {
    let mut keys = Keys::new("- a\n  - b\n  - c\n- d").at(1, 0);
    keys.keys("dd");
    assert_eq!(keys.markdown(), "- a\n    - c\n- d");
    assert_eq!(keys.host.editor.document().blocks[1].depth, 1);
}

// ---------------------------------------------------------------- yank and paste

#[test]
fn a_linewise_yank_pastes_below_and_above_as_whole_lines() {
    let mut keys = Keys::new("- **one**\ntwo").at(0, 0);
    keys.keys("yy");
    assert_eq!(keys.cursor(), at(0, 0));
    keys.keys("jp");
    assert_eq!(keys.markdown(), "- **one**\ntwo\n- **one**");
    keys.keys("P");
    assert_eq!(keys.markdown(), "- **one**\ntwo\n- **one**\n- **one**");
}

#[test]
fn a_charwise_yank_pastes_inline_after_and_at_the_cursor() {
    let mut keys = Keys::new("abcd").at(0, 0);
    keys.keys("ylp");
    assert_eq!(keys.text(), "aabcd");
    assert_eq!(keys.cursor(), at(0, 1));
    let mut keys = Keys::new("abcd").at(0, 0);
    keys.keys("ylP");
    assert_eq!(keys.text(), "aabcd");
}

#[test]
fn a_delete_fills_the_register_so_dd_then_p_moves_a_line() {
    let mut keys = Keys::new("one\ntwo\nthree").at(0, 0);
    keys.keys("ddjp");
    assert_eq!(keys.text(), "two\nthree\none");
}

#[test]
fn a_clipboard_from_elsewhere_pastes_inline_rather_than_as_lines() {
    let mut keys = Keys::new("one\ntwo").at(0, 0);
    keys.keys("yy");
    // Something else took the clipboard: the linewise flag no longer applies.
    keys.host.clipboard = Some(Document::from_markdown("X"));
    keys.keys("p");
    assert_eq!(keys.text(), "oXne\ntwo");
}

#[test]
fn paste_of_a_multi_block_linewise_register_keeps_every_kind() {
    let mut keys = Keys::new("# head\n- [ ] task\nlast").at(0, 0);
    keys.keys("2yy");
    keys.keys("G");
    keys.keys("p");
    assert_eq!(
        keys.markdown(),
        "# head\n- [ ] task\nlast\n# head\n- [ ] task"
    );
}

// ---------------------------------------------------------------- insert mode

#[test]
fn the_insert_commands_place_the_caret_where_vim_does() {
    let mut keys = Keys::new("  hello").at(0, 4);
    keys.keys("i");
    assert_eq!(keys.host.editor.selection().head, at(0, 4));
    keys.keys("<esc>");
    assert_eq!(keys.cursor(), at(0, 3));
    keys.keys("a");
    assert_eq!(keys.host.editor.selection().head, at(0, 4));
    keys.keys("<esc>I");
    assert_eq!(keys.host.editor.selection().head, at(0, 2));
    keys.keys("<esc>A");
    assert_eq!(keys.host.editor.selection().head, at(0, 7));
}

#[test]
fn escape_from_insert_steps_left_but_not_off_the_start_of_the_line() {
    let mut keys = Keys::new("ab").at(0, 0);
    keys.keys("i").typed("XY").keys("<esc>");
    assert_eq!(keys.text(), "XYab");
    assert_eq!(keys.cursor(), at(0, 1));
    let mut keys = Keys::new("ab").at(0, 0);
    keys.keys("<esc>");
    assert_eq!(keys.cursor(), at(0, 0));
    // Appending leaves the caret past the last grapheme; escape still steps only one.
    let mut keys = Keys::new("ab").at(0, 0);
    keys.keys("A").typed("XY").keys("<esc>");
    assert_eq!(keys.cursor(), at(0, 3));
    keys.keys("i").typed("Z").keys("<esc>");
    assert_eq!(keys.text(), "abXZY");
}

#[test]
fn o_and_shift_o_open_the_line_enter_would_make() {
    let mut keys = Keys::new("- [x] done\n# head").at(0, 0);
    keys.keys("o").typed("next").keys("<esc>");
    assert_eq!(keys.markdown(), "- [x] done\n- [ ] next\n# head");
    keys.keys("G");
    keys.keys("O").typed("above").keys("<esc>");
    assert_eq!(keys.markdown(), "- [x] done\n- [ ] next\nabove\n# head");
}

#[test]
fn o_inside_a_code_run_opens_another_line_of_the_same_language() {
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
    assert_eq!(keys.cursor(), at(0, 3));
    keys.keys("d");
    assert_eq!(keys.text(), "aef");
    assert_eq!(keys.state.mode, Mode::Normal);
}

#[test]
fn a_backwards_visual_selection_covers_both_ends() {
    let mut keys = Keys::new("abcdef").at(0, 4);
    keys.keys("vhh");
    assert_eq!(keys.selected(), "cde");
    assert_eq!(keys.cursor(), at(0, 2));
    keys.keys("y");
    assert_eq!(keys.cursor(), at(0, 2));
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
fn visual_line_mode_takes_whole_blocks_in_both_directions() {
    let mut keys = Keys::new("# one\ntwo\n- three\nfour").at(1, 0);
    keys.keys("V");
    assert_eq!(keys.selected(), "two");
    keys.keys("j");
    assert_eq!(keys.selected(), "two\nthree");
    keys.keys("d");
    assert_eq!(keys.markdown(), "# one\nfour");

    let mut keys = Keys::new("one\ntwo\nthree").at(2, 0);
    keys.keys("Vky");
    assert!(keys.state.register.as_ref().expect("a yank").linewise);
    keys.keys("Gp");
    assert_eq!(keys.text(), "one\ntwo\nthree\ntwo\nthree");
}

#[test]
fn v_and_shift_v_toggle_off_and_switch_between_each_other() {
    let mut keys = Keys::new("abc\ndef").at(0, 1);
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
    assert_eq!(keys.cursor(), at(0, 0));
}

#[test]
fn escape_leaves_a_visual_mode_without_changing_the_document() {
    let mut keys = Keys::new("abc").at(0, 0);
    keys.keys("vl<esc>");
    assert_eq!(keys.state.mode, Mode::Normal);
    assert_eq!(keys.text(), "abc");
    assert_eq!(keys.cursor(), at(0, 1));
}

#[test]
fn a_visual_change_leaves_insert_mode_with_the_selection_gone() {
    let mut keys = Keys::new("one two").at(0, 0);
    keys.keys("vllc");
    assert_eq!(keys.state.mode, Mode::Insert);
    assert_eq!(keys.text(), " two");
}

#[test]
fn a_visual_selection_spans_blocks() {
    let mut keys = Keys::new("abc\ndef").at(0, 1);
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
        .typed("\n")
        .typed("two")
        .typed("\n")
        .typed("# ")
        .typed("head");
    keys.keys("<esc>");
    assert_eq!(keys.markdown(), "- item one\n- two\n# head");
    keys.keys("u");
    assert_eq!(keys.markdown(), "- item");
    keys.keys("r");
    assert_eq!(keys.markdown(), "- item one\n- two\n# head");

    // The edit that opened the session belongs to it: `o` and `cw` with their text.
    let mut keys = Keys::new("one two").at(0, 0);
    keys.keys("o").typed("below").keys("<esc>");
    assert_eq!(keys.text(), "one two\nbelow");
    keys.keys("u");
    assert_eq!(keys.text(), "one two");
    keys.keys("cw").typed("uno").keys("<esc>");
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
fn a_linewise_change_is_one_step_even_across_several_blocks() {
    let mut keys = Keys::new("one\ntwo\nthree").at(0, 0);
    keys.keys("2cc");
    assert_eq!(keys.text(), "\nthree");
    keys.keys("<esc>u");
    assert_eq!(keys.text(), "one\ntwo\nthree");
}

#[test]
fn a_paste_undoes_in_one_step() {
    let mut keys = Keys::new("one\ntwo").at(0, 0);
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
    assert_eq!(keys.cursor(), at(0, 4));
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
    assert_eq!(keys.keys("l").cursor(), at(0, 3));
    assert_eq!(keys.keys("$").cursor(), at(0, 6));
    keys.keys("0");
    keys.keys("2x");
    assert_eq!(keys.text(), "字");
}

#[test]
fn a_charwise_yank_across_blocks_keeps_its_marks_when_pasted_back() {
    let mut keys = Keys::new("a **bold** c\nsecond").at(0, 2);
    keys.keys("v$y");
    assert_eq!(keys.state.register.as_ref().expect("a yank").text, "bold c");
    keys.keys("G$p");
    assert_eq!(keys.markdown(), "a **bold** c\nsecond**bold** c");
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
fn dd_on_the_only_block_leaves_an_empty_paragraph_and_undoes() {
    let mut keys = Keys::new("# only").at(0, 0);
    keys.keys("dd");
    assert_eq!(keys.host.editor.document(), &Document::default());
    keys.keys("p");
    assert_eq!(keys.markdown(), "\n# only");
    keys.keys("u");
    assert_eq!(keys.markdown(), "");
    keys.keys("u");
    assert_eq!(keys.markdown(), "# only");
}

#[test]
fn a_paste_into_a_code_line_stays_literal() {
    let mut keys = Keys::new("**bold**\n```rust\nx\n```").at(0, 0);
    keys.keys("v$y");
    keys.keys("j$p");
    assert_eq!(keys.markdown(), "**bold**\n```rust\nxbold\n```");
    assert!(!keys.host.editor.document().blocks[1].spans[0].marks.bold);
}
