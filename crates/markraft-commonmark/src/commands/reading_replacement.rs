//! Reading-text replacements over source-backed styled paragraphs.
//!
//! The diff addresses reading graphemes, never concealed syntax. Equal units
//! keep their styles; inserted units inherit the edited location. Paragraph
//! boundaries carry the following paragraph's identity, and a new boundary
//! belongs to the paragraph in which it was inserted.

use super::*;
use markraft_core::kind::ReadingReplacementPolicy;
use similar::{Algorithm, DiffTag, capture_diff_slices_deadline};
use std::time::{Duration, Instant};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone)]
struct Token {
    unit: Option<Unit>,
    origin: Option<(usize, usize)>,
    paragraph: usize,
    source: Range<usize>,
}

struct SourcePatch {
    old_units: Range<usize>,
    new_units: Range<usize>,
    source: Range<usize>,
    items: Vec<Item>,
}

impl Token {
    fn character(&self) -> Option<char> {
        match self.unit.as_ref().map(|unit| &unit.content) {
            Some(Content::Char(character)) => Some(*character),
            Some(Content::Break { .. }) | None => Some('\n'),
            Some(Content::Atom(_)) => None,
        }
    }
}

pub(super) fn owns_structure(state: &EditorState, selected: Range<usize>) -> bool {
    let mut position = 0;
    state.doc().children().all(|node| {
        let end = position + node.node_size();
        let touches = selected.start < end && selected.end > position
            || selected.is_empty() && selected.start > position && selected.start < end;
        position = end;
        !touches || state.schema().node_type(node.type_id()).name() == md::PARAGRAPH
    })
}

pub(super) fn replace(
    state: &EditorState,
    selected: Range<usize>,
    replacement: &str,
    policy: ReadingReplacementPolicy,
    house: HouseStyle,
) -> Option<TransactionSpec> {
    if selected.start > selected.end || selected.end > state.doc().content_size() {
        return None;
    }
    let schema = state.schema();
    let ctx = document_context(schema, state.doc());
    let mut blocks = Vec::new();
    let mut offset = 0;
    for node in state.doc().children() {
        let end = offset + node.node_size();
        if selected.start < end && selected.end > offset
            || selected.is_empty() && selected.start > offset && selected.start < end
        {
            let block = Block::read(schema, &ctx, node, offset + 1)?;
            if block.kind != BlockKind::Paragraph {
                return None;
            }
            blocks.push(block);
        }
        offset = end;
    }
    let first = blocks.first()?;
    let last = blocks.last()?;
    let mut old = Vec::new();
    for (paragraph, block) in blocks.iter().enumerate() {
        if paragraph > 0 {
            let previous = &blocks[paragraph - 1];
            old.push(Token {
                unit: None,
                origin: None,
                paragraph,
                source: previous.start + previous.items.len()..block.start,
            });
        }
        for (index, unit) in block.units.iter().enumerate() {
            old.push(Token {
                unit: Some(unit.clone()),
                origin: Some((paragraph, index)),
                paragraph,
                source: block.start + unit.src.start..block.start + unit.src.end,
            });
        }
    }
    // Unselected atoms are opaque source-owned units. Only the selected slice
    // must consist entirely of reading characters; neighboring images survive.
    let old_text: String = old
        .iter()
        .map(|token| token.character().unwrap_or('\u{fffc}'))
        .collect();
    let start = old.partition_point(|token| token.source.end <= selected.start);
    let end = old.partition_point(|token| token.source.start < selected.end);
    if start > end
        || old[start..end].iter().any(|token| {
            token.source.start < selected.start
                || token.source.end > selected.end
                || token
                    .unit
                    .as_ref()
                    .is_some_and(|unit| unit.styles.iter().any(is_literal))
        })
    {
        return None;
    }
    let boundaries = scalar_boundaries(&old_text);
    if !boundaries.contains(&start) || !boundaries.contains(&end) {
        return None;
    }
    // A caret inside hidden syntax is not a reading insertion point.
    if start == end && !old.is_empty() {
        let at = old.get(start).map(|token| token.source.start);
        let before = start.checked_sub(1).map(|index| old[index].source.end);
        if at != Some(selected.start) && before != Some(selected.start) {
            return None;
        }
    }
    let selected_text: String = old[start..end]
        .iter()
        .map(Token::character)
        .collect::<Option<_>>()?;
    let replacement = replacement.replace("\r\n", "\n").replace('\r', "\n");
    if policy == ReadingReplacementPolicy::PreserveUnchanged && selected_text == replacement {
        return Some(TransactionSpec::new());
    }
    let inserted = match policy {
        ReadingReplacementPolicy::PreserveUnchanged => {
            diff_replacement(&old, start, &selected_text, &replacement, selected.start)
        }
        ReadingReplacementPolicy::InheritSelectionStart => {
            let inherited = if start < end {
                old.get(start)
                    .filter(|token| token.unit.is_some())
                    .or_else(|| start.checked_sub(1).and_then(|index| old.get(index)))
            } else {
                start
                    .checked_sub(1)
                    .and_then(|index| old.get(index))
                    .filter(|token| token.unit.is_some())
                    .or_else(|| old.get(start))
            };
            let paragraph = inherited.map_or(0, |token| token.paragraph);
            let styles = inherited
                .and_then(|token| token.unit.as_ref())
                .map_or_else(Vec::new, |unit| unit.styles.clone());
            replacement
                .chars()
                .map(|character| Token {
                    unit: (character != '\n').then(|| Unit {
                        content: Content::Char(character),
                        styles: styles.clone(),
                        src: 0..0,
                    }),
                    origin: None,
                    paragraph,
                    source: selected.start..selected.start,
                })
                .collect()
        }
    };
    let replacement_end = start + inserted.len();
    let mut target = old[..start].to_vec();
    target.extend(inserted);
    target.extend_from_slice(&old[end..]);
    let mut groups = vec![(0, Vec::new())];
    for token in &target {
        if token.unit.is_none() {
            groups.push((token.paragraph, Vec::new()));
        } else {
            groups.last_mut()?.1.push(token.clone());
        }
    }
    let mut nodes = Vec::new();
    let mut reading_spans = Vec::new();
    let mut position = first.start;
    let mut previous_end = first.start;
    let mut first_caret = first.start;
    for (index, (owner, tokens)) in groups.iter().enumerate() {
        let block = &blocks[*owner];
        let (node, read) = render(schema, block, *owner, tokens, house)?;
        let empty_caret = position;
        if index == 0 {
            first_caret = empty_caret;
        }
        if index > 0 {
            let next_start = read
                .first()
                .map_or(empty_caret, |unit| position + unit.src.start);
            reading_spans.push(previous_end..next_start);
        }
        reading_spans.extend(
            read.iter()
                .map(|unit| position + unit.src.start..position + unit.src.end),
        );
        previous_end = read
            .last()
            .map_or(empty_caret, |unit| position + unit.src.end);
        position += node.node_size();
        nodes.push(node);
    }
    let target_text: String = target
        .iter()
        .map(|token| token.character().unwrap_or('\u{fffc}'))
        .collect();
    let boundaries = scalar_boundaries(&target_text);
    // A newly inserted combining mark can join an unselected neighbor. Native
    // selections must enclose that new grapheme, never point into its interior.
    let selection_start = if start == replacement_end {
        *boundaries.iter().find(|boundary| **boundary >= start)?
    } else {
        *boundaries
            .iter()
            .rev()
            .find(|boundary| **boundary <= start)?
    };
    let selection_end = *boundaries
        .iter()
        .find(|boundary| **boundary >= replacement_end)?;
    let target_from = reading_spans
        .get(selection_start)
        .map(|span| span.start)
        .or_else(|| reading_spans.last().map(|span| span.end))
        .unwrap_or(first_caret);
    let target_to = selection_end
        .checked_sub(1)
        .filter(|_| selection_end > selection_start)
        .and_then(|index| reading_spans.get(index))
        .map_or(target_from, |span| span.end);
    let selection = if policy == ReadingReplacementPolicy::InheritSelectionStart {
        Selection::cursor(target_to)
    } else if start == replacement_end {
        Selection::cursor(target_from)
    } else {
        Selection::text(target_from, target_to)
    };
    let spec = TransactionSpec::new()
        .changes([Change::replace(
            first.start - 1,
            last.start + last.node.content_size() + 1,
            Slice::from_fragment(Fragment::from_nodes(nodes.clone())),
        )])
        .selection(selection)
        .user_event(event::INPUT_REPLACE)
        .scroll_into_view();
    // Corrections may reinterpret Markdown at structural boundaries. Require
    // the actual corrected paragraphs to equal the validated intended nodes.
    let transaction = state.update([spec.clone()]).ok()?;
    let mut actual = Vec::new();
    transaction
        .new_doc()
        .nodes_between(first.start - 1, position - 1, &mut |node, _, _, _| {
            actual.push(node.clone());
            false
        });
    if actual.len() != nodes.len() {
        return None;
    }
    for (actual, wanted) in actual.iter().zip(&nodes) {
        if actual.type_id() != wanted.type_id()
            || Items::from_nodes(schema, actual.children())
                != Items::from_nodes(schema, wanted.children())
        {
            return None;
        }
    }
    Some(spec)
}

fn diff_replacement(
    old: &[Token],
    start: usize,
    selected_text: &str,
    replacement: &str,
    source_start: usize,
) -> Vec<Token> {
    let original_graphemes = selected_text.graphemes(true).collect::<Vec<_>>();
    let new_graphemes = replacement.graphemes(true).collect::<Vec<_>>();
    let original_offsets = scalar_boundaries(selected_text);
    let mut inserted = Vec::new();
    // Similar's deadline fallback emits a valid coarser replacement for an
    // unresolved region. It never blocks the UI trying to prove a minimal diff.
    let deadline = Instant::now() + Duration::from_millis(12);
    for operation in capture_diff_slices_deadline(
        Algorithm::Myers,
        &original_graphemes,
        &new_graphemes,
        Some(deadline),
    ) {
        let old_range = operation.old_range();
        let old_start = start + original_offsets[old_range.start];
        let old_end = start + original_offsets[old_range.end];
        if operation.tag() == DiffTag::Equal {
            inserted.extend_from_slice(&old[old_start..old_end]);
            continue;
        }
        // A replacement inherits its first removed character. An insertion
        // uses the preceding character unless it follows a paragraph boundary.
        let inherited = if old_start < old_end {
            old.get(old_start)
                .filter(|token| token.unit.is_some())
                .or_else(|| old_start.checked_sub(1).and_then(|index| old.get(index)))
        } else {
            old_start
                .checked_sub(1)
                .and_then(|index| old.get(index))
                .filter(|token| token.unit.is_some())
                .or_else(|| old.get(old_start))
        };
        let paragraph = inherited.map_or(0, |token| token.paragraph);
        let styles = inherited
            .and_then(|token| token.unit.as_ref())
            .map_or_else(Vec::new, |unit| unit.styles.clone());
        for grapheme in &new_graphemes[operation.new_range()] {
            for character in grapheme.chars() {
                inserted.push(Token {
                    unit: (character != '\n').then(|| Unit {
                        content: Content::Char(character),
                        styles: styles.clone(),
                        src: 0..0,
                    }),
                    origin: None,
                    paragraph,
                    source: source_start..source_start,
                });
            }
        }
    }
    inserted
}

fn scalar_boundaries(text: &str) -> Vec<usize> {
    let mut position = 0;
    let mut boundaries = vec![0];
    for grapheme in text.graphemes(true) {
        position += grapheme.chars().count();
        boundaries.push(position);
    }
    boundaries
}

fn render(
    schema: &Schema,
    block: &Block,
    owner: usize,
    tokens: &[Token],
    house: HouseStyle,
) -> Option<(Node, Vec<Unit>)> {
    if tokens.len() == block.units.len()
        && tokens
            .iter()
            .enumerate()
            .all(|(index, token)| token.origin == Some((owner, index)))
    {
        return Some((block.node.clone(), block.units.clone()));
    }
    let units = tokens
        .iter()
        .map(|token| token.unit.clone())
        .collect::<Option<Vec<_>>>()?;
    let target = units
        .iter()
        .map(|unit| unit.styles.clone())
        .collect::<Vec<_>>();
    // Prefer a bounded source rewrite, retaining untouched entity spellings
    // and reference links byte for byte.
    let local = local_source(schema, block, owner, tokens, house).or_else(|| {
        // Budget exhaustion merges the selected changes into one source patch;
        // keep the unselected prefix/suffix instead of respelling the paragraph.
        tokens
            .iter()
            .all(|token| token.origin.is_none_or(|(paragraph, _)| paragraph == owner))
            .then(|| local_single(schema, block, owner, tokens, &units, house))
            .flatten()
    });
    if let Some(items) = local {
        let derived = derive(block.kind, &items.text(), &block.ctx);
        let read = units_of(&items, &derived);
        if check(&units, &target, &read).is_ok() {
            return Some((
                block
                    .node
                    .copy(Fragment::from_nodes(items_to_nodes(schema, &items.0))),
                read,
            ));
        }
    }
    let (source, atoms) = spell_units(
        schema,
        block,
        units
            .iter()
            .map(|unit| (&unit.content, unit.styles.as_slice())),
        &[],
        0,
        true,
        house,
    )
    .ok()?;
    let items = Items(spelled_items(&source, atoms).ok()?);
    let derived = derive(block.kind, &items.text(), &block.ctx);
    let read = units_of(&items, &derived);
    check(&units, &target, &read).is_ok().then(|| {
        (
            block
                .node
                .copy(Fragment::from_nodes(items_to_nodes(schema, &items.0))),
            read,
        )
    })
}

fn local_source(
    schema: &Schema,
    block: &Block,
    owner: usize,
    tokens: &[Token],
    house: HouseStyle,
) -> Option<Items> {
    if tokens.iter().any(|token| {
        token
            .origin
            .is_some_and(|(paragraph, _)| paragraph != owner)
    }) {
        return None;
    }
    // Plan disjoint changed stretches independently. A proofread sentence may
    // fix its first and last word while the reference-link or entity spelling
    // in between must remain byte-for-byte intact.
    let original = block
        .units
        .iter()
        .enumerate()
        .map(|(index, unit)| Token {
            unit: Some(unit.clone()),
            origin: Some((owner, index)),
            paragraph: owner,
            source: block.start + unit.src.start..block.start + unit.src.end,
        })
        .collect::<Vec<_>>();
    // Each local patch rederives its whole paragraph. Bound that cumulative
    // work; larger edits use one validated whole-paragraph spelling instead.
    let mut remaining_work = 256 * 1024usize;
    let mut patch = |old_range: Range<usize>, new_range: Range<usize>| {
        remaining_work = remaining_work.checked_sub(block.items.len().max(1))?;
        let mut partial = original[..old_range.start].to_vec();
        partial.extend_from_slice(&tokens[new_range.clone()]);
        partial.extend_from_slice(&original[old_range.end..]);
        let partial_units = partial
            .iter()
            .filter_map(|token| token.unit.clone())
            .collect::<Vec<_>>();
        let items = local_single(schema, block, owner, &partial, &partial_units, house)?;
        let kept = KeptEnds::of(&block.items.0, &items.0, |a, b| a == b);
        Some(SourcePatch {
            old_units: old_range,
            new_units: new_range,
            source: kept.old_middle(),
            items: items.0[kept.new_middle()].to_vec(),
        })
    };
    let mut changes: Vec<SourcePatch> = Vec::new();
    let (mut old_at, mut new_at) = (0, 0);
    while old_at < original.len() || new_at < tokens.len() {
        if tokens
            .get(new_at)
            .is_some_and(|token| token.origin == Some((owner, old_at)))
        {
            old_at += 1;
            new_at += 1;
            continue;
        }
        let next = tokens[new_at..]
            .iter()
            .enumerate()
            .find_map(|(index, token)| {
                token
                    .origin
                    .map(|(_, index_old)| (new_at + index, index_old))
            })
            .unwrap_or((tokens.len(), original.len()));
        let mut change = patch(old_at..next.1, new_at..next.0)?;
        while changes
            .last()
            .is_some_and(|previous| previous.source.end > change.source.start)
        {
            let previous = changes.pop()?;
            change = patch(
                previous.old_units.start..change.old_units.end,
                previous.new_units.start..change.new_units.end,
            )?;
        }
        changes.push(change);
        (old_at, new_at) = (next.1, next.0);
    }
    let mut items = block.items.0.clone();
    for change in changes.into_iter().rev() {
        items.splice(change.source, change.items);
    }
    Some(Items(items))
}

fn local_single(
    schema: &Schema,
    block: &Block,
    owner: usize,
    tokens: &[Token],
    units: &[Unit],
    house: HouseStyle,
) -> Option<Items> {
    let prefix = tokens
        .iter()
        .enumerate()
        .take_while(|(index, token)| token.origin == Some((owner, *index)))
        .count();
    let suffix = tokens[prefix..]
        .iter()
        .rev()
        .zip((prefix..block.units.len()).rev())
        .take_while(|(token, index)| token.origin == Some((owner, *index)))
        .count();
    let mut lo = block.units.get(prefix).map_or_else(
        || block.units.last().map_or(0, |unit| unit.src.end),
        |unit| unit.src.start,
    );
    let mut hi = block
        .units
        .len()
        .checked_sub(suffix + 1)
        .and_then(|index| block.units.get(index))
        .map_or(lo, |unit| unit.src.end)
        .max(lo);
    let mut context;
    loop {
        context = Vec::new();
        let mut grew = false;
        for span in &block.derived.styles {
            if span.range.end < lo || span.range.start > hi {
                continue;
            }
            let (open, close) = delimiters(&block.derived, span);
            if open.end <= lo
                && close.start >= hi
                && units[prefix..units.len() - suffix]
                    .iter()
                    .all(|unit| unit.styles.contains(&span.style))
            {
                context.push(span.style.clone());
            } else if span.range.start < lo || span.range.end > hi {
                lo = lo.min(span.range.start);
                hi = hi.max(span.range.end);
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    let inside = tokens
        .iter()
        .filter(|token| match token.origin {
            Some((_, index)) => {
                block.units[index].src.start >= lo && block.units[index].src.end <= hi
            }
            None => true,
        })
        .filter_map(|token| token.unit.as_ref())
        .collect::<Vec<_>>();
    let (source, atoms) = spell_units(
        schema,
        block,
        inside
            .iter()
            .map(|unit| (&unit.content, unit.styles.as_slice())),
        &context,
        lo,
        true,
        house,
    )
    .ok()?;
    let mut items = block.items.0[..lo].to_vec();
    items.extend(spelled_items(&source, atoms).ok()?);
    items.extend_from_slice(&block.items.0[hi..]);
    let items = Items(items);
    let read = units_of(&items, &derive(block.kind, &items.text(), &block.ctx));
    let target = units
        .iter()
        .map(|unit| unit.styles.clone())
        .collect::<Vec<_>>();
    check(units, &target, &read).is_ok().then_some(items)
}
