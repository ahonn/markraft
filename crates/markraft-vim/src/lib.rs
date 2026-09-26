//! Modal editing for the Markraft editor, as an opt-in extension.
//!
//! Register [`vim`] on a note editor and call [`bind_keys`] once, after
//! `markraft_gpui::bind_keys`. With no instance registered nothing changes: every
//! binding here is predicated on a `vim_mode` the editor's key context only carries
//! while an instance is alive.
//!
//! # What a line is
//!
//! Vim's "line" is a projection line: one textblock, or one block-level leaf such as a
//! horizontal rule. That decides the rest:
//!
//! - `0`, `^` and `$` act on the whole line, not on the visual row a wrapped line
//!   occupies. `j` and `k` do follow visual rows, as they do in vim with `wrap` set —
//!   except with an operator pending and in Visual Line mode, where `dj` and `Vj` must
//!   take whole lines however they wrap.
//! - A linewise yank takes the outermost node the lines fill completely, so yanking the
//!   only paragraph of a nested task item takes the item and pasting it gives back a
//!   nested task. `p` and `P` put those nodes below and above the cursor's own node at
//!   the same depth. A charwise yank keeps a slice and pastes it inline, after the
//!   cursor's grapheme for `p` and at it for `P`.
//! - A horizontal rule holds no text. The cursor may rest on it, `dd` removes it, `x`
//!   finds nothing to delete, and `o` or `O` beside it opens a paragraph.
//! - A code block is one line whose text holds the newlines, so `dd` and `yy` on it take
//!   the whole block; `o` and `O` inside it open another row of the same block. No fence
//!   is ever written or stripped.
//! - `o` and `O` are Enter: the caret goes to the end — or the start — of the line and
//!   the editor's own Enter chain runs, so a list item repeats itself, a heading gives a
//!   paragraph and a code block gains a row.
//! - Deleting everything leaves the smallest document the schema allows.
//! - In Normal mode the cursor sits on a grapheme, never past the last one of a
//!   non-empty line; it is clamped after every command and whenever the editor moves it.
//! - A table is the one place where the line is not the projection line: there it is
//!   the row, which the next section is about.
//!
//! # Tables
//!
//! The projection gives every cell a line of its own, in row-major order, but vim's
//! line inside a table is the *row*. A row is what can be taken out and put back; a row
//! with fewer cells than the header is a table no content rule can reject, no command
//! can put right and nothing but undo can undo.
//!
//! - `dd` takes the row, and the table with it when it is the only one. `yy` takes the
//!   row whole, and `p` and `P` put one back below and above the cursor's row. That
//!   same register pasted where there is no table becomes a table of its own, and
//!   anything that is not a row pasted inside one lands beside the table.
//! - `cc` is the exception: it clears the *cell*. Emptying every cell of a row is a
//!   great deal to ask of a keystroke that in vim never leaves the text it is on, and
//!   `dd` is there for the row itself.
//! - `o` and `O` add a row below or above and begin Insert mode in the same column.
//!   They are not Enter here: Enter inside a cell steps to the row below and appends
//!   one at the bottom of the table, which is right for Enter and not for `o`.
//! - `j` and `k` step rows, keeping the column, and leave the table at its edges
//!   rather than appending a row there; `h` and `l` step into the cell beside when the
//!   cursor is at a cell's own edge, and stop at the table's; `w`, `b` and `e` cross
//!   cells exactly as they cross lines; `0`, `^` and `$` act on the cell; `gg` and `G`
//!   count lines, so in a table they count cells.
//! - A count counts rows: `2dd` takes two of them, and stops at the last one rather
//!   than reaching out of the grid.
//! - `V` selects the whole row. `d`, `c` and `x` refuse a charwise selection that
//!   spans two cells — the one edit that would merge them — and leave both the
//!   document and the selection alone; a yank changes nothing and is free to span it.
//!
//! # Words
//!
//! `w`, `b` and `e` use the editor's own word boundaries — UAX#29 segments that are not
//! wholly whitespace, the same ones ⌥← and double-click use. That is close to vim's `w`
//! and never to its `W`: punctuation runs are their own words, as in vim, but a word is
//! split wherever Unicode says so, so `can't` is three and CJK text breaks by script
//! rather than running to the next space. `w`, `b` and `e` cross block boundaries;
//! `dw` on the last word of a line stops at the line's end rather than pulling the next
//! line up, as it does in vim.
//!
//! # Concealed spelling
//!
//! A kind that keeps its markup in the text — Markdown's `**`, a backslash
//! escape — conceals it until the cursor reaches its span. Motions move over
//! what a reader sees:
//!
//! - `w`, `b` and `e` go from word to word of the text and never rest on
//!   markup, concealed or revealed: `w` from `x` in `x **bold** y` lands on
//!   `b`, `e` on `d`.
//! - `h`, `l` and `x` take a run the cursor leaves concealed as one step, and
//!   no motion stops inside one. Landing on its start reveals it, as the caret
//!   reaching a span does outside vim, and from there the cursor walks its
//!   characters like any others — the way to edit the spelling itself, which
//!   `x` takes as it comes. A count takes its steps one at a time, so `2x`
//!   before `**b**` takes the space and then one `*`, as `xx` does.
//! - An operator over a range keeps markup whole: a span whose text the range
//!   takes goes with its spelling, and one it only reaches into keeps every
//!   run of it, so `dw` on `bold` leaves `x y` and `D` from its `o` leaves
//!   `x **b**`. A yank takes the same, so a word yanked whole pastes styled.
//!   A change keeps even the spelling it empties, so `cw` on `bold` types a
//!   new bold word; Escape with nothing typed takes the empty pair away.
//!
//! # Registers
//!
//! There is one register, the unnamed one, and it is the system clipboard: a yank or a
//! delete writes the rich fragment and its plain text there, so `yy` then ⌘V in another
//! application works. `p` reads the clipboard, and treats it as linewise only while it
//! still holds exactly what was last yanked — so ⌘C in another application pastes
//! inline, as it should.
//!
//! # The editor's own keys
//!
//! In Normal and the visual modes, Backspace, Delete and Return would otherwise edit the
//! document under a caret that is resting on a character, so they are rebound to `h`, `x`
//! and `j`. Everything else the editor and the app bind keeps its meaning: the arrow
//! keys, Tab and ⇧Tab still indent, and ⌘K, ⌘P, ⌘N, ⌘Z and the rest are untouched, vim
//! binding no modified keys but ⌃R. Escape in Normal mode with nothing half-typed still
//! reaches the app, which closes whatever it has open and otherwise points to `:q`
//! rather than hiding the window.
//!
//! In Insert mode nothing here applies but Escape, so the typeahead menus, the emoji
//! shortcodes and every editor binding behave exactly as they do with vim off.
//!
//! # Text objects
//!
//! `iw aw`, the quote pairs `i" i' i``, the bracket pairs `i( i[ i{ i<` with `b` and
//! `B`, and `ip ap`, each with its `a` form, after an operator or in a visual mode; see
//! [`object`]. The count before one is consumed but not applied.
//!
//! # Not implemented
//!
//! `.`, `J`, marks, macros, named registers and search. `:` is left to the host, which
//! binds it to a command list of its own. Nothing joins two lines, so nothing can join
//! two table rows either.

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

mod command;
mod edit;
mod host;
mod motion;
mod object;
mod state;
mod table;
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests;

use command::InsertAt;
use gpui::{App, KeyBinding, KeyContext, actions};
use markraft_gpui::{ActionHandler, CaretShape, EditorCx, Extension, InputPolicy, Update};
use motion::Motion;
use object::TextObject;
use state::{Operator, State};
use std::{cell::RefCell, rc::Rc};

pub use state::Mode;

/// The extension id: the origin of every edit vim makes and the id on the
/// `EditorEvent::Extension` that reports the mode.
pub const VIM: &str = "vim";

actions!(
    markraft_vim,
    [
        // Modes.
        VimNormal,
        VimClearPending,
        VimInsert,
        VimInsertAfter,
        VimInsertLineStart,
        VimInsertLineEnd,
        VimOpenBelow,
        VimOpenAbove,
        VimVisual,
        VimVisualLine,
        // Motions.
        VimLeft,
        VimRight,
        VimDown,
        VimUp,
        VimWordForward,
        VimWordBackward,
        VimWordEnd,
        VimFirstNonBlank,
        VimLineEnd,
        VimDocumentStart,
        VimDocumentEnd,
        // Operators and commands.
        VimDelete,
        VimChange,
        VimYank,
        VimDeleteChar,
        VimDeleteToLineEnd,
        VimChangeToLineEnd,
        VimPasteAfter,
        VimPasteBefore,
        VimUndo,
        VimRedo,
        // Text objects, after an operator or in a visual mode.
        VimInnerWord,
        VimAroundWord,
        VimInnerDoubleQuote,
        VimAroundDoubleQuote,
        VimInnerSingleQuote,
        VimAroundSingleQuote,
        VimInnerBacktick,
        VimAroundBacktick,
        VimInnerParen,
        VimAroundParen,
        VimInnerBracket,
        VimAroundBracket,
        VimInnerBrace,
        VimAroundBrace,
        VimInnerAngle,
        VimAroundAngle,
        VimInnerParagraph,
        VimAroundParagraph,
        // Counts. `0` is a count only after another digit; on its own it is the
        // line-start motion, which its handler performs instead.
        VimCount0,
        VimCount1,
        VimCount2,
        VimCount3,
        VimCount4,
        VimCount5,
        VimCount6,
        VimCount7,
        VimCount8,
        VimCount9,
    ]
);

/// Everywhere a bare letter is a command rather than text: Normal mode, the
/// operator-pending state inside it, and both visual modes.
///
/// `vim_mode` on its own tests that the key is present at all. `!=` alone would not:
/// gpui reads a missing key as "not equal", which is exactly the state with vim
/// disabled.
const COMMAND: Option<&str> = Some("Markraft && vim_mode && vim_mode != insert");
const NORMAL: Option<&str> = Some("Markraft && vim_mode == normal");
/// `i` and `a` start a text object once an operator is waiting, so as commands of their
/// own they stand aside then.
const NORMAL_UNARMED: Option<&str> = Some("Markraft && vim_mode == normal && !vim_operator");
/// Where a text object can follow: an operator waiting for its range, or a visual mode.
const OBJECT: Option<&str> =
    Some("Markraft && (vim_operator || vim_mode == visual || vim_mode == visual_line)");
const VISUAL: Option<&str> = Some("Markraft && (vim_mode == visual || vim_mode == visual_line)");
/// Escape leaves Insert mode only when it has nothing better to do: an open typeahead
/// menu takes it first to close itself, and a live composition belongs to the input
/// method, which the editor's own `escape` binding cancels.
const INSERT: Option<&str> = Some("Markraft && vim_mode == insert && !typeahead && !vim_composing");
/// Escape with half a command typed forgets it instead of reaching the app.
const PENDING: Option<&str> = Some("Markraft && vim_pending");

/// Register vim's key bindings. Call it once, after `markraft_gpui::bind_keys`, so that
/// at the editor's context depth these win over the editor's own bindings and over the
/// typeahead's — which matters only for the keys whose predicates deliberately stand
/// aside, `escape` above all.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys(bindings());
}

/// Every binding [`bind_keys`] registers, in the order it registers them.
fn bindings() -> Vec<KeyBinding> {
    let mut bindings = Vec::new();
    macro_rules! bind { ($context:ident { $($key:literal => $action:ident),* $(,)? }) => {
        bindings.extend([$(KeyBinding::new($key, $action, $context)),*]);
    }; }
    bind!(COMMAND {
        "h" => VimLeft, "l" => VimRight, "j" => VimDown, "k" => VimUp,
        "w" => VimWordForward, "b" => VimWordBackward, "e" => VimWordEnd,
        "^" => VimFirstNonBlank, "$" => VimLineEnd,
        "g g" => VimDocumentStart, "shift-g" => VimDocumentEnd,
        "d" => VimDelete, "c" => VimChange, "y" => VimYank,
        "x" => VimDeleteChar,
        "shift-d" => VimDeleteToLineEnd, "shift-c" => VimChangeToLineEnd,
        "0" => VimCount0, "1" => VimCount1, "2" => VimCount2, "3" => VimCount3,
        "4" => VimCount4, "5" => VimCount5, "6" => VimCount6, "7" => VimCount7,
        "8" => VimCount8, "9" => VimCount9,
        // The editor's own meanings would edit the document under a Normal-mode caret.
        "backspace" => VimLeft, "delete" => VimDeleteChar, "enter" => VimDown,
    });
    bind!(NORMAL_UNARMED { "i" => VimInsert, "a" => VimInsertAfter });
    bind!(OBJECT {
        "i w" => VimInnerWord, "a w" => VimAroundWord,
        "i \"" => VimInnerDoubleQuote, "a \"" => VimAroundDoubleQuote,
        "i '" => VimInnerSingleQuote, "a '" => VimAroundSingleQuote,
        "i `" => VimInnerBacktick, "a `" => VimAroundBacktick,
        "i (" => VimInnerParen, "a (" => VimAroundParen,
        "i )" => VimInnerParen, "a )" => VimAroundParen,
        "i b" => VimInnerParen, "a b" => VimAroundParen,
        "i [" => VimInnerBracket, "a [" => VimAroundBracket,
        "i ]" => VimInnerBracket, "a ]" => VimAroundBracket,
        "i {" => VimInnerBrace, "a {" => VimAroundBrace,
        "i }" => VimInnerBrace, "a }" => VimAroundBrace,
        "i shift-b" => VimInnerBrace, "a shift-b" => VimAroundBrace,
        "i <" => VimInnerAngle, "a <" => VimAroundAngle,
        "i >" => VimInnerAngle, "a >" => VimAroundAngle,
        "i p" => VimInnerParagraph, "a p" => VimAroundParagraph,
    });
    bind!(NORMAL {
        "shift-i" => VimInsertLineStart, "shift-a" => VimInsertLineEnd,
        "o" => VimOpenBelow, "shift-o" => VimOpenAbove,
        "v" => VimVisual, "shift-v" => VimVisualLine,
        "p" => VimPasteAfter, "shift-p" => VimPasteBefore,
        "u" => VimUndo, "ctrl-r" => VimRedo,
    });
    bind!(VISUAL {
        "v" => VimVisual, "shift-v" => VimVisualLine,
        "escape" => VimNormal,
    });
    bind!(INSERT { "escape" => VimNormal });
    // Last, so that in Normal mode a half-typed command takes escape back from the app.
    bind!(PENDING { "escape" => VimClearPending });
    bindings
}

/// What one action does, over any [`host::Host`]: the extension runs it on the
/// editor, and the tests run the very same on a bare state, so there is one vim.
#[derive(Clone, Copy)]
enum Command {
    Motion(Motion),
    Object(TextObject, bool),
    /// A digit: part of a count, or on its own `0` the line-start motion.
    Count(usize),
    Vertical(isize),
    DocumentStart,
    DocumentEnd,
    Operator(Operator),
    DeleteChar,
    ToLineEnd(Operator),
    Paste {
        after: bool,
    },
    History {
        undo: bool,
    },
    Insert(InsertAt),
    Open {
        below: bool,
    },
    Visual {
        linewise: bool,
    },
    Normal,
    ClearPending,
}

impl Command {
    fn run(self, state: &mut State, cx: &mut impl host::Host) {
        match self {
            Command::Motion(motion) => command::motion(state, cx, motion),
            Command::Object(object, around) => command::text_object(state, cx, object, around),
            Command::Count(digit) => {
                if !state.pending.digit(digit) {
                    // A leading `0` is the line-start motion rather than a count.
                    command::motion(state, cx, Motion::LineStart);
                }
            }
            Command::Vertical(delta) => command::vertical(state, cx, delta),
            // `gg` and `G` read a count as the line to go to, counting from one, so it
            // is not consumed as a repetition; `command::motion` takes the rest.
            Command::DocumentStart => {
                let line = state.pending.count().map_or(0, |count| count - 1);
                command::motion(state, cx, Motion::Line(line));
            }
            Command::DocumentEnd => {
                let last = cx.projection().line_count().saturating_sub(1);
                let line = state.pending.count().map_or(last, |count| count - 1);
                command::motion(state, cx, Motion::Line(line));
            }
            Command::Operator(operator) => command::operator(state, cx, operator),
            Command::DeleteChar => command::delete_chars(state, cx),
            Command::ToLineEnd(operator) => command::to_line_end(state, cx, operator),
            Command::Paste { after } => command::paste(state, cx, after),
            Command::History { undo } => command::history(state, cx, undo),
            Command::Insert(at) => command::insert(state, cx, at),
            Command::Open { below } => command::open_line(state, cx, below),
            Command::Visual { linewise } => command::visual(state, cx, linewise),
            Command::Normal => command::normal(state, cx),
            Command::ClearPending => command::clear_pending(state, cx),
        }
    }
}

/// Every action and the command it runs. It makes the extension's handlers, and
/// [`command_of`], which the tests read a bound action back through.
macro_rules! commands { ($($action:ident => $command:expr),* $(,)?) => {
    fn handlers(vim: &Vim) -> Vec<ActionHandler> {
        vec![$(vim.handler($action, $command)),*]
    }

    /// The command `action` runs, when it is one of vim's.
    #[cfg(test)]
    fn command_of(action: &dyn gpui::Action) -> Option<Command> {
        $(if gpui::Action::partial_eq(&$action, action) {
            return Some($command);
        })*
        None
    }
}; }

commands! {
    VimLeft => Command::Motion(Motion::Left),
    VimRight => Command::Motion(Motion::Right),
    VimWordForward => Command::Motion(Motion::WordForward),
    VimWordBackward => Command::Motion(Motion::WordBackward),
    VimWordEnd => Command::Motion(Motion::WordEnd),
    VimFirstNonBlank => Command::Motion(Motion::FirstNonBlank),
    VimLineEnd => Command::Motion(Motion::LineEnd),
    VimDown => Command::Vertical(1),
    VimUp => Command::Vertical(-1),
    VimDocumentStart => Command::DocumentStart,
    VimDocumentEnd => Command::DocumentEnd,
    VimInnerWord => Command::Object(TextObject::Word, false),
    VimAroundWord => Command::Object(TextObject::Word, true),
    VimInnerDoubleQuote => Command::Object(TextObject::Quote('"'), false),
    VimAroundDoubleQuote => Command::Object(TextObject::Quote('"'), true),
    VimInnerSingleQuote => Command::Object(TextObject::Quote('\''), false),
    VimAroundSingleQuote => Command::Object(TextObject::Quote('\''), true),
    VimInnerBacktick => Command::Object(TextObject::Quote('`'), false),
    VimAroundBacktick => Command::Object(TextObject::Quote('`'), true),
    VimInnerParen => Command::Object(TextObject::Pair('(', ')'), false),
    VimAroundParen => Command::Object(TextObject::Pair('(', ')'), true),
    VimInnerBracket => Command::Object(TextObject::Pair('[', ']'), false),
    VimAroundBracket => Command::Object(TextObject::Pair('[', ']'), true),
    VimInnerBrace => Command::Object(TextObject::Pair('{', '}'), false),
    VimAroundBrace => Command::Object(TextObject::Pair('{', '}'), true),
    VimInnerAngle => Command::Object(TextObject::Pair('<', '>'), false),
    VimAroundAngle => Command::Object(TextObject::Pair('<', '>'), true),
    VimInnerParagraph => Command::Object(TextObject::Paragraph, false),
    VimAroundParagraph => Command::Object(TextObject::Paragraph, true),
    VimDelete => Command::Operator(Operator::Delete),
    VimChange => Command::Operator(Operator::Change),
    VimYank => Command::Operator(Operator::Yank),
    VimDeleteChar => Command::DeleteChar,
    VimDeleteToLineEnd => Command::ToLineEnd(Operator::Delete),
    VimChangeToLineEnd => Command::ToLineEnd(Operator::Change),
    VimPasteAfter => Command::Paste { after: true },
    VimPasteBefore => Command::Paste { after: false },
    VimUndo => Command::History { undo: true },
    VimRedo => Command::History { undo: false },
    VimInsert => Command::Insert(InsertAt::Cursor),
    VimInsertAfter => Command::Insert(InsertAt::AfterCursor),
    VimInsertLineStart => Command::Insert(InsertAt::FirstNonBlank),
    VimInsertLineEnd => Command::Insert(InsertAt::LineEnd),
    VimOpenBelow => Command::Open { below: true },
    VimOpenAbove => Command::Open { below: false },
    VimVisual => Command::Visual { linewise: false },
    VimVisualLine => Command::Visual { linewise: true },
    VimNormal => Command::Normal,
    VimClearPending => Command::ClearPending,
    VimCount0 => Command::Count(0),
    VimCount1 => Command::Count(1),
    VimCount2 => Command::Count(2),
    VimCount3 => Command::Count(3),
    VimCount4 => Command::Count(4),
    VimCount5 => Command::Count(5),
    VimCount6 => Command::Count(6),
    VimCount7 => Command::Count(7),
    VimCount8 => Command::Count(8),
    VimCount9 => Command::Count(9),
}

/// The keys vim adds to the editor's context for `state`; see [`Extension::key_context`].
fn describe(state: &State, context: &mut KeyContext) {
    context.set("vim_mode", state.mode.context());
    // Outside insert mode keys are commands, which the editor's own
    // text-editing bindings have to yield to.
    if state.mode != crate::state::Mode::Insert {
        context.add("modal");
    }
    if let Some(operator) = state.pending.operator() {
        context.set("vim_operator", operator.context());
    }
    if !state.pending.is_empty() {
        context.add("vim_pending");
    }
    if state.composing {
        context.add("vim_composing");
    }
}

/// Modal editing over one editor. Starts in Normal mode.
#[derive(Default)]
pub struct Vim {
    state: Rc<RefCell<State>>,
}

/// A vim instance to register with `EditorView::add_extension`.
pub fn vim() -> Vim {
    Vim::default()
}

impl Vim {
    /// One vim command. A live composition belongs to the input method, so no command
    /// runs under one; every command redraws, because the mode, the pending operator and
    /// the caret's shape all reach the screen through the key context and the caret.
    fn handler(&self, action: impl gpui::Action, command: Command) -> ActionHandler {
        let state = self.state.clone();
        ActionHandler::new(action, move |cx: &mut EditorCx<'_>| {
            if cx.is_composing() {
                return;
            }
            let reported = {
                let mut state = state.borrow_mut();
                command.run(&mut state, cx);
                state.report()
            };
            if let Some(mode) = reported {
                cx.emit(Rc::new(mode));
            }
            cx.notify();
        })
    }
}

impl Extension for Vim {
    fn id(&self) -> &'static str {
        VIM
    }

    /// `vim_mode` carries the mode; `vim_operator` the operator waiting for a motion;
    /// `vim_pending` marks any half-typed command, count included; `vim_composing` marks
    /// a live input-method composition, which keeps Escape away from vim.
    fn key_context(&self, context: &mut KeyContext) {
        describe(&self.state.borrow(), context);
    }

    /// Outside Insert mode the editor takes no text, which is also what keeps `j`, `d`
    /// and `:` reaching key bindings while a Chinese or Japanese input source is active.
    fn input_policy(&self) -> Option<InputPolicy> {
        Some(match self.state.borrow().mode {
            Mode::Insert => InputPolicy::Accept,
            _ => InputPolicy::Refuse,
        })
    }

    fn caret(&self) -> Option<CaretShape> {
        Some(match self.state.borrow().mode {
            Mode::Insert => CaretShape::Bar,
            _ => CaretShape::Block,
        })
    }

    fn update(&mut self, update: &Update, cx: &mut EditorCx<'_>) {
        let reported = {
            let mut state = self.state.borrow_mut();
            if state.composing != update.composing {
                state.composing = update.composing;
                cx.notify();
            }
            if state.composing || state.mode == Mode::Insert {
                state.report()
            } else {
                command::settle(&mut state, cx, update.replaced)
            }
        };
        if let Some(mode) = reported {
            cx.emit(Rc::new(mode));
        }
    }

    fn actions(&self) -> Vec<ActionHandler> {
        handlers(self)
    }
}
