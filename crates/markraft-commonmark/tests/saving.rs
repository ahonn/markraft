//! Random notes, spelled by hand, edited and saved through [`SourceDocument`].
//!
//! A note spells its blocks every way Markdown allows — setext headings, `+`
//! bullets, padded tables, indented code, lazy quote lines, CRLF endings —
//! and an edit must not respell what it did not touch. For each random note:
//!
//! * saving it unedited writes the file back byte for byte;
//! * a word retyped in the file itself, the way another editor would, saves as
//!   exactly that file;
//! * edits made through the editor's own commands save, the file reads back as
//!   the edited note, and every top-level block before and after the ones the
//!   edit touched keeps its bytes.

mod common;

use common::Rng;
use markraft_commonmark::schema as md;
use markraft_commonmark::{
    SourceDocument, commonmark_extensions, commonmark_schema, from_markdown, to_markdown,
};
use markraft_core::commands::{
    Command, Direction, TableTypes, add_row_after, chain, delete_by_grapheme, delete_row,
    delete_selection, insert_text, join_backward, run_command,
};
use markraft_core::{EditorState, EditorStateConfig, Fragment, Node, Schema, Selection};

/// Plain words: typing one next to anything leaves every other spelling as it
/// was, so the edits below never change what the text around them means.
const WORDS: &[&str] = &[
    "alpha", "beta", "gamma", "delta", "omega", "kappa", "sigma", "theta", "中文", "zeta",
];

/// One top-level block of a note, spelled by hand.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Paragraph,
    Heading,
    List,
    Quote,
    Code,
    Table,
    Rule,
    Raw,
}

struct Note {
    /// The whole file.
    source: String,
    /// Each block's bytes in `source`, in document order.
    blocks: Vec<std::ops::Range<usize>>,
    kinds: Vec<Kind>,
}

fn word(rng: &mut Rng) -> &'static str {
    WORDS[rng.below(WORDS.len())]
}

fn words(rng: &mut Rng, least: usize, most: usize) -> String {
    let count = rng.range(least, most);
    (0..count).map(|_| word(rng)).collect::<Vec<_>>().join(" ")
}

fn block(rng: &mut Rng) -> (Kind, String) {
    match rng.below(20) {
        0 | 1 => (Kind::Paragraph, words(rng, 1, 5)),
        2 => (
            Kind::Paragraph,
            format!("{}\n{}", words(rng, 1, 3), words(rng, 1, 3)),
        ),
        3 => (
            Kind::Paragraph,
            format!(
                "{} *{}* **{}** `{}` [{}](/url) {}",
                word(rng),
                word(rng),
                word(rng),
                word(rng),
                word(rng),
                word(rng)
            ),
        ),
        4 => {
            let brk = if rng.one_in(2) { "  " } else { "\\" };
            (
                Kind::Paragraph,
                format!("{}{brk}\n{}", words(rng, 1, 3), words(rng, 1, 3)),
            )
        }
        5 => (
            Kind::Heading,
            format!("{} {}", "#".repeat(rng.range(1, 6)), words(rng, 1, 3)),
        ),
        6 => {
            let underline = if rng.one_in(2) { '=' } else { '-' };
            let length = rng.range(1, 8);
            (
                Kind::Heading,
                format!(
                    "{}\n{}",
                    words(rng, 1, 3),
                    underline.to_string().repeat(length)
                ),
            )
        }
        7 => {
            let marker = *rng.pick(&['-', '+', '*']);
            let space = " ".repeat(rng.range(1, 3));
            let loose = rng.one_in(3);
            let items: Vec<_> = (0..rng.range(1, 3))
                .map(|_| format!("{marker}{space}{}", words(rng, 1, 3)))
                .collect();
            (Kind::List, items.join(if loose { "\n\n" } else { "\n" }))
        }
        8 => {
            let delimiter = *rng.pick(&['.', ')']);
            let start = rng.range(1, 9);
            let items: Vec<_> = (0..rng.range(1, 3))
                .map(|index| format!("{}{delimiter} {}", start + index, words(rng, 1, 3)))
                .collect();
            (Kind::List, items.join("\n"))
        }
        9 => (
            Kind::List,
            format!(
                "- {}\n  - {}\n- {}",
                words(rng, 1, 2),
                words(rng, 1, 2),
                words(rng, 1, 2)
            ),
        ),
        10 => (
            Kind::List,
            format!("- [ ] {}\n- [x] {}", words(rng, 1, 2), words(rng, 1, 2)),
        ),
        11 => {
            let prefix = if rng.one_in(3) { ">" } else { "> " };
            let lines: Vec<_> = (0..rng.range(1, 3))
                .map(|_| format!("{prefix}{}", words(rng, 1, 3)))
                .collect();
            (Kind::Quote, lines.join("\n"))
        }
        12 => (
            Kind::Quote,
            format!("> {}\n{}", words(rng, 1, 2), words(rng, 1, 2)),
        ),
        13 => (
            Kind::Quote,
            format!("> [!note] {}\n> {}", word(rng), words(rng, 1, 3)),
        ),
        14 => {
            let fence = if rng.one_in(2) { "```" } else { "~~~" };
            let language = *rng.pick(&["", "rust", "text"]);
            (
                Kind::Code,
                format!(
                    "{fence}{language}\n{}\n{}\n{fence}",
                    words(rng, 1, 3),
                    words(rng, 0, 2)
                ),
            )
        }
        15 => (
            Kind::Code,
            format!("    {}\n    {}", words(rng, 1, 3), words(rng, 1, 2)),
        ),
        16 => {
            let rows = rng.range(1, 3);
            let padded = rng.one_in(2);
            let row = |cells: [&str; 2]| {
                if padded {
                    format!("| {:<6} | {:<6} |", cells[0], cells[1])
                } else {
                    format!("|{}|{}|", cells[0], cells[1])
                }
            };
            let mut lines = vec![row(["a", "b"])];
            lines.push(if padded {
                "| ------ | :----: |".into()
            } else {
                "|-|:-:|".into()
            });
            for _ in 0..rows {
                lines.push(row([word(rng), word(rng)]));
            }
            (Kind::Table, lines.join("\n"))
        }
        17 => (
            Kind::Rule,
            rng.pick(&["***", "- - -", "___", "-----"]).to_string(),
        ),
        18 => (Kind::Raw, format!("<div>\n{}\n</div>", words(rng, 1, 2))),
        _ => (
            Kind::Paragraph,
            format!("{} [{}][ref] {}", word(rng), word(rng), word(rng)),
        ),
    }
}

fn note(rng: &mut Rng) -> Note {
    let newline = if rng.one_in(3) { "\r\n" } else { "\n" };
    let mut body = String::new();
    let mut spans = Vec::new();
    let mut kinds: Vec<Kind> = Vec::new();
    let count = rng.range(1, 6);
    let mut references = false;
    let mut last_indented = false;
    while kinds.len() < count {
        let (kind, text) = block(rng);
        // Two lists in a row are one list to a reader, an indented line after
        // a list continues its last item, and two indented code blocks are one.
        let indented = text.starts_with("    ");
        if (kinds.last() == Some(&Kind::List) && (kind == Kind::List || indented))
            || (indented && last_indented)
        {
            continue;
        }
        last_indented = indented;
        references |= text.contains("[ref]");
        if !kinds.is_empty() {
            body.push_str(&"\n".repeat(rng.range(2, 4)));
        }
        spans.push(body.len()..body.len() + text.len());
        body.push_str(&text);
        kinds.push(kind);
    }
    if references {
        body.push_str("\n\n");
        let text = "[ref]: /target \"Title\"";
        spans.push(body.len()..body.len() + text.len());
        body.push_str(text);
        kinds.push(Kind::Paragraph);
    }
    body.push_str(&"\n".repeat(rng.range(0, 2)));
    let front = if rng.one_in(4) {
        "---\ntitle: note\n---\n"
    } else {
        ""
    };
    let convert = |text: &str| text.replace('\n', newline);
    let source = convert(&format!("{front}{body}"));
    let shift = |at: usize| convert(&format!("{front}{}", &body[..at])).len();
    let blocks = spans
        .into_iter()
        .map(|span| shift(span.start)..shift(span.end))
        .collect();
    Note {
        source,
        blocks,
        kinds,
    }
}

/// Whether `saved` reads as `target`, as far as a file can say: spaces ending
/// a paragraph and an empty paragraph are typing in progress that a file does
/// not hold.
fn reads_as(schema: &Schema, saved: &str, target: &Node) -> bool {
    let read = SourceDocument::parse(schema, saved).expect("a saved file parses");
    let read = read.document();
    let bare = markraft_commonmark::source::without_empty_paragraphs;
    let trimmed = without_trailing_spaces(schema, target);
    let markdown = |node: &Node| to_markdown(schema, node);
    read == target
        || markdown(read) == markdown(target)
        || markdown(read) == markdown(&trimmed)
        || markdown(&bare(schema, read)) == markdown(&bare(schema, &trimmed))
}

/// `node` without the spaces and tabs a reader drops from the end of a
/// paragraph, a heading or a table cell.
fn without_trailing_spaces(schema: &Schema, node: &Node) -> Node {
    if node.is_text() || node.child_count() == 0 {
        return node.clone();
    }
    let mut children: Vec<_> = node
        .children()
        .map(|child| without_trailing_spaces(schema, child))
        .collect();
    let trims = [md::PARAGRAPH, md::HEADING, md::TABLE_CELL]
        .iter()
        .any(|name| schema.node_id(name) == Some(node.type_id()));
    while trims && let Some(text) = children.last().and_then(Node::text) {
        let kept = text.trim_end_matches([' ', '\t']);
        if !kept.is_empty() {
            let last = children.last().unwrap().with_text(kept);
            *children.last_mut().unwrap() = last;
            break;
        }
        children.pop();
    }
    node.copy(Fragment::from_nodes(children))
}

fn parsed(schema: &Schema, note: &Note, seed: u64) -> SourceDocument {
    let source = SourceDocument::parse(schema, &note.source).expect("a note parses");
    assert_eq!(
        source.document().child_count(),
        note.blocks.len(),
        "seed {seed}: the generator's blocks were read otherwise\n{:?}",
        note.source
    );
    source
}

#[test]
fn an_unedited_note_saves_byte_for_byte() {
    let schema = commonmark_schema();
    for seed in 1..400u64 {
        let note = note(&mut Rng::new(seed));
        let source = parsed(&schema, &note, seed);
        assert_eq!(
            source.render(&schema, source.document()).unwrap(),
            note.source,
            "seed {seed}"
        );
        // A document equal to the note in all but identity is the note too.
        let again = from_markdown(&schema, &to_markdown(&schema, source.document())).unwrap();
        if to_markdown(&schema, &again) == to_markdown(&schema, source.document()) {
            assert_eq!(
                source.render(&schema, &again).unwrap(),
                note.source,
                "seed {seed}"
            );
        }
    }
}

/// Retyping a word in the file, as another editor would, and opening the
/// result: saving that note must write exactly the file that was typed, not a
/// respelling of the block around it.
#[test]
fn a_word_retyped_in_the_file_saves_as_that_file() {
    let schema = commonmark_schema();
    for seed in 1..800u64 {
        let mut rng = Rng::new(seed);
        let note = note(&mut rng);
        let source = parsed(&schema, &note, seed);
        // Every word the generator wrote, found by its bytes, for each block
        // that holds text.
        let mut blocks = Vec::new();
        for (index, range) in note.blocks.iter().enumerate() {
            let mut places = Vec::new();
            for word in WORDS {
                for (at, _) in note.source[range.clone()].match_indices(word) {
                    places.push((range.start + at, word.len()));
                }
            }
            if note.kinds[index] != Kind::Rule && !places.is_empty() {
                places.sort_unstable();
                blocks.push(places);
            }
        }
        if blocks.is_empty() {
            continue;
        }
        for _ in 0..3 {
            // One word, or two in the same block, as one save.
            let places = rng.pick(&blocks);
            let mut chosen = vec![*rng.pick(places)];
            let other = *rng.pick(places);
            if rng.one_in(2) && other != chosen[0] {
                chosen.push(other);
                chosen.sort_unstable();
            }
            let mut typed = note.source.clone();
            for &(at, length) in chosen.iter().rev() {
                let replacement = match rng.below(3) {
                    0 => word(&mut rng).to_string(),
                    1 => format!("{} {}", &note.source[at..at + length], word(&mut rng)),
                    _ => format!("{}{}", &note.source[at..at + length], word(&mut rng)),
                };
                typed.replace_range(at..at + length, &replacement);
            }
            let target = SourceDocument::parse(&schema, &typed).unwrap();
            let saved = source
                .render(&schema, target.document())
                .unwrap_or_else(|error| panic!("seed {seed}: {error}\n{typed:?}"));
            assert_eq!(saved, typed, "seed {seed}: from {:?}", note.source);
        }
    }
}

/// An edit the editor makes, at a random place in a random textblock.
enum Edit {
    Type,
    Enter,
    Backspace,
    DeleteWithin,
    DeleteAcross,
    AddRow,
    DeleteRow,
}

const EDITS: &[Edit] = &[
    Edit::Type,
    Edit::Type,
    Edit::Type,
    Edit::Enter,
    Edit::Backspace,
    Edit::DeleteWithin,
    Edit::DeleteWithin,
    Edit::DeleteAcross,
    Edit::AddRow,
    Edit::DeleteRow,
];

/// Each textblock's content range, and whether it is a table cell.
fn textblocks(schema: &Schema, doc: &Node) -> Vec<(usize, usize, bool)> {
    let cell = schema.node_id(md::TABLE_CELL).unwrap();
    let mut found = Vec::new();
    doc.descendants(&mut |node, pos, _, _| {
        if node.is_textblock(schema) {
            found.push((
                pos + 1,
                pos + 1 + node.content_size(),
                node.type_id() == cell,
            ));
            return false;
        }
        true
    });
    found
}

fn enter(schema: &Schema) -> Command {
    use markraft_core::commands::*;
    let item = schema.node_id(md::LIST_ITEM).unwrap();
    chain(vec![
        split_list_item(item),
        lift_list_item(item),
        new_line_in_code(),
        create_paragraph_near(),
        lift_empty_block(),
        split_block_keep_marks(),
    ])
}

fn table_types(schema: &Schema) -> TableTypes {
    TableTypes::new(
        schema.node_id(md::TABLE).unwrap(),
        schema.node_id(md::TABLE_ROW).unwrap(),
        schema.node_id(md::TABLE_CELL).unwrap(),
        "alignments",
    )
}

/// `state` with `edit` made somewhere, or `None` where it does not apply.
fn edited(schema: &Schema, state: &EditorState, rng: &mut Rng, edit: &Edit) -> Option<EditorState> {
    let doc = state.doc();
    let blocks = textblocks(schema, doc);
    if blocks.is_empty() {
        return None;
    }
    let &(from, to, in_cell) = rng.pick(&blocks);
    let at = rng.range(from, to);
    let (selection, command) = match edit {
        Edit::Type => {
            let text = format!(" {}", word(rng));
            let mut current = state
                .update([markraft_core::TransactionSpec::new().selection(Selection::cursor(at))])
                .ok()?
                .state()
                .clone();
            for character in text.chars() {
                current = run_command(&current, &insert_text(&character.to_string()))?
                    .ok()?
                    .state()
                    .clone();
            }
            return Some(current);
        }
        Edit::Enter if !in_cell => (Selection::cursor(at), enter(schema)),
        Edit::Backspace if !in_cell => (
            Selection::cursor(if rng.one_in(2) { from } else { at }),
            chain(vec![
                delete_selection(),
                delete_by_grapheme(Direction::Backward),
                join_backward(),
            ]),
        ),
        Edit::DeleteWithin if at < to => (
            Selection::text(at, rng.range(at + 1, to)),
            delete_selection(),
        ),
        Edit::DeleteAcross if !in_cell => {
            let &(_, end, cell) = rng.pick(&blocks);
            if cell || end <= to {
                return None;
            }
            (Selection::text(at, rng.range(to, end)), delete_selection())
        }
        Edit::AddRow if in_cell => (Selection::cursor(at), add_row_after(table_types(schema))),
        Edit::DeleteRow if in_cell => (Selection::cursor(at), delete_row(table_types(schema))),
        _ => return None,
    };
    let moved = state
        .update([markraft_core::TransactionSpec::new().selection(selection)])
        .ok()?
        .state()
        .clone();
    Some(run_command(&moved, &command)?.ok()?.state().clone())
}

/// Top-level blocks the two documents share at their start and at their end.
fn shared_ends(old: &Node, new: &Node) -> (usize, usize) {
    let old: Vec<_> = old.children().collect();
    let new: Vec<_> = new.children().collect();
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    (prefix, suffix)
}

#[test]
fn editor_edits_save_and_leave_untouched_blocks_alone() {
    let schema = commonmark_schema();
    let mut saved_edits = 0;
    for seed in 1..1500u64 {
        let mut rng = Rng::new(seed);
        let note = note(&mut rng);
        let source = parsed(&schema, &note, seed);
        let mut state = EditorState::create(
            EditorStateConfig::new(schema.clone())
                .doc(source.document().clone())
                .selection(Selection::cursor(1))
                .extensions(commonmark_extensions(&schema)),
        )
        .expect("a valid state");
        let mut made = 0;
        for _ in 0..rng.range(1, 3) {
            let edit = rng.pick(EDITS);
            if let Some(next) = edited(&schema, &state, &mut rng, edit) {
                state = next;
                made += 1;
            }
        }
        if made == 0 || state.doc() == source.document() {
            continue;
        }
        let target = state.doc();
        // An edit can leave a note no Markdown file holds — a paragraph that
        // opens with a space — which the editor refuses before it gets here.
        // Whatever the writer can write, a save must manage too.
        if !reads_as(&schema, &to_markdown(&schema, target), target) {
            continue;
        }
        let saved = source.render(&schema, target).unwrap_or_else(|error| {
            panic!(
                "seed {seed}: {error}\n{:?}\n{}",
                note.source,
                schema.describe(target)
            )
        });
        assert!(
            reads_as(&schema, &saved, target),
            "seed {seed}: {saved:?} does not read as the edited note\n{}",
            schema.describe(target)
        );
        let (prefix, suffix) = shared_ends(source.document(), target);
        let old = source.document().child_count();
        if prefix > 0 {
            let kept = &note.source[..note.blocks[prefix - 1].end];
            assert!(
                saved.starts_with(kept),
                "seed {seed}: the blocks before the edit changed\n{:?}\n{saved:?}",
                note.source
            );
        }
        if suffix > 0 {
            let kept = &note.source[note.blocks[old - suffix].start..];
            assert!(
                saved.ends_with(kept),
                "seed {seed}: the blocks after the edit changed\n{:?}\n{saved:?}",
                note.source
            );
        }
        saved_edits += 1;
    }
    assert!(saved_edits > 800, "only {saved_edits} notes were edited");
}
