//! A visible-text selection whose source crosses concealed spelling.
//! The range remains the visual selection; only replacement owns delimiter
//! balancing, through the same concealment contract used by reading edits.
use std::any::Any;

use serde_json::{Value, json};

use crate::{
    ChangeDesc, MarkTypeId, Node, NodeError, ReplacementStyle, Schema, Selection, SelectionKind,
    Slice, TransactionSpec,
};

const TAG: &str = "reading-text";

#[derive(Debug, Clone, PartialEq, Eq)]
/// Visible text whose source requires balanced delimiter replacement.
pub struct ReadingSelection {
    anchor: usize,
    head: usize,
    syntax: MarkTypeId,
}

impl ReadingSelection {
    /// Select visible content across concealed spelling. Explicit source
    /// selections remain ordinary text selections; this kind owns balanced
    /// replacement without widening the visual range. An empty range is a
    /// cursor.
    pub fn selection(anchor: usize, head: usize, syntax: MarkTypeId) -> Selection {
        if anchor == head {
            Selection::cursor(head)
        } else {
            Selection::custom(Box::new(Self {
                anchor,
                head,
                syntax,
            }))
        }
    }

    /// Whether `selection` is a range of visible text across source spelling.
    pub fn is(selection: &Selection) -> bool {
        matches!(selection, Selection::Custom(kind) if kind.as_any().is::<Self>())
    }

    /// Read back what [`Selection::to_json`] wrote for this kind. The syntax
    /// mark resolves by its schema name.
    ///
    /// # Errors
    ///
    /// Fails when `value` is not this kind's shape or names an unknown mark.
    pub fn from_json(schema: &Schema, value: &Value) -> Result<Selection, NodeError> {
        let number = |name: &str| {
            value
                .get(name)
                .and_then(Value::as_u64)
                .map(|n| n as usize)
                .ok_or_else(|| NodeError::Json(format!("a selection needs `{name}`")))
        };
        if value.get("type").and_then(Value::as_str) != Some(TAG) {
            return Err(NodeError::Json(format!("a selection of type `{TAG}`")));
        }
        let syntax = value
            .get("syntax")
            .and_then(Value::as_str)
            .and_then(|name| schema.mark_id(name))
            .ok_or_else(|| NodeError::Json(format!("{TAG} needs a known syntax mark")))?;
        Ok(Self::selection(number("anchor")?, number("head")?, syntax))
    }
}

impl SelectionKind for ReadingSelection {
    fn tag(&self) -> &str {
        TAG
    }
    fn clone_box(&self) -> Box<dyn SelectionKind> {
        Box::new(self.clone())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn eq_kind(&self, other: &dyn SelectionKind) -> bool {
        other.as_any().downcast_ref::<Self>() == Some(self)
    }
    fn anchor(&self, _: &Node) -> usize {
        self.anchor
    }
    fn head(&self, _: &Node) -> usize {
        self.head
    }
    /// Without a schema the ends cannot be checked against inline content;
    /// [`Selection::map`] takes [`Self::map_with_schema`] instead.
    fn map(&self, doc: &Node, changes: &ChangeDesc) -> Selection {
        let map = |position| {
            changes
                .map_pos(position, 1, Default::default())
                .unwrap_or(doc.content_size())
                .min(doc.content_size())
        };
        Self::selection(map(self.anchor), map(self.head), self.syntax)
    }
    fn map_with_schema(&self, schema: &Schema, doc: &Node, changes: &ChangeDesc) -> Selection {
        match Selection::text(self.anchor, self.head).map(schema, doc, changes) {
            Selection::Text { anchor, head } => Self::selection(anchor, head, self.syntax),
            other => other,
        }
    }
    fn to_json(&self, schema: &Schema) -> Value {
        json!({"anchor":self.anchor, "head":self.head, "syntax":schema.mark_type(self.syntax).name()})
    }
    fn content_with_schema(&self, doc: &Node, schema: &Schema) -> Slice {
        let range = self.replacement_range(doc);
        doc.slice_with_schema(schema, range.from, range.to)
            .unwrap_or_else(|_| Slice::empty())
    }
    fn replace_with_schema(
        &self,
        spec: TransactionSpec,
        doc: &Node,
        schema: &Schema,
        slice: Slice,
        style: ReplacementStyle,
    ) -> TransactionSpec {
        let projection = crate::projection::Projection::of(doc, schema);
        let selected = self.replacement_range(doc);
        let range = selected.from..selected.to;
        let inherit = style == ReplacementStyle::Receiving;
        let ranges = if slice.is_empty() || !inherit {
            crate::kind::conceal::markup_safe(&projection, Some(self.syntax), range, false)
        } else {
            crate::kind::conceal::markup_replacement(&projection, Some(self.syntax), range)
        };
        let (slice, ranges) = if inherit {
            continue_paragraph_styles(
                doc,
                schema,
                &projection,
                self.syntax,
                selected.from..selected.to,
                slice,
                ranges,
            )
        } else {
            (slice, ranges)
        };
        let Some(first) = ranges.first() else {
            return spec;
        };
        // Keep the source range's style inheritance at the command boundary.
        // An emptied inner span goes away; a partially retained span keeps its
        // complete spelling instead of leaving an orphan opening/closing tag.
        let mut changes =
            crate::commands::replace_selection_changes(schema, doc, first.start, first.end, &slice);
        changes.extend(ranges.iter().skip(1).flat_map(|range| {
            crate::commands::delete_range_changes(schema, doc, range.start, range.end)
        }));
        // Additional protected gaps are after the primary insertion. Explicit
        // caret mapping therefore follows its end rather than the original
        // visual selection's far edge.
        if let Ok(set) = crate::ChangeSet::create(schema, doc, changes.clone())
            && let Ok(next) = set.apply(doc)
        {
            let caret = set
                .map_pos(first.end, 1, Default::default())
                .unwrap_or(next.content_size());
            return spec
                .change_set(set)
                .selection(Selection::near(schema, &next, caret, -1));
        }
        spec.changes(changes)
    }
    fn check(&self, doc: &Node, schema: &Schema) -> Result<(), NodeError> {
        Selection::text(self.anchor, self.head).check(doc, schema)?;
        if schema.try_mark_type(self.syntax).is_none() {
            return Err(NodeError::Json(
                "reading selection has an unknown syntax mark".into(),
            ));
        }
        Ok(())
    }
}

/// Literal paragraph insertion must not stretch source delimiters across block
/// boundaries. Continue the same enclosing styles in each nonempty paragraph,
/// retaining original boundary spelling only when it still encloses content.
fn continue_paragraph_styles(
    doc: &Node,
    schema: &Schema,
    projection: &crate::projection::Projection,
    syntax: MarkTypeId,
    selected: std::ops::Range<usize>,
    slice: Slice,
    mut ranges: Vec<std::ops::Range<usize>>,
) -> (Slice, Vec<std::ops::Range<usize>>) {
    use crate::{
        Fragment,
        kind::conceal::{self, Reveal, Shown},
    };
    if slice.open_start() != 1
        || slice.open_end() != 1
        || slice.content().child_count() < 2
        || !slice
            .content()
            .iter()
            .all(|node| schema.node_type(node.type_id()).is_textblock())
    {
        return (slice, ranges);
    }
    let Some(line) = projection
        .line_at(selected.start)
        .and_then(|index| projection.line(index))
    else {
        return (slice, ranges);
    };
    let visible = conceal::shown(Some(syntax), line, &Reveal::nothing());
    let has_content = |from: usize, to: usize| {
        from < to
            && line.runs().iter().zip(&visible).any(|(run, shown)| {
                line.abs(run.start) < to
                    && from < line.abs(run.end)
                    && match shown {
                        Shown::Hidden => false,
                        Shown::Display(text) => !text.is_empty(),
                        Shown::Source | Shown::Revealed => true,
                    }
            })
    };
    let pairs = conceal::markup_spans(Some(syntax), line)
        .into_iter()
        .filter_map(|runs| {
            let first = runs.first()?.clone();
            let last = runs.last()?.clone();
            (runs.len() > 1 && first.end <= selected.start && selected.start < last.start)
                .then_some((first, last))
        })
        .collect::<Vec<_>>();
    if pairs.is_empty() {
        return (slice, ranges);
    }
    let first_has_text = slice
        .content()
        .first_child()
        .is_some_and(|node| node.content_size() > 0);
    let last_has_text = slice
        .content()
        .last_child()
        .is_some_and(|node| node.content_size() > 0);
    let mut openings = Vec::new();
    let mut closings = Vec::new();
    let mut first_closings = Vec::new();
    let mut last_openings = Vec::new();
    for (opening, closing) in pairs {
        if first_has_text || has_content(opening.end, selected.start) {
            first_closings.push(closing.clone());
        } else {
            ranges.push(opening.clone());
        }
        if last_has_text || has_content(selected.end, closing.start) {
            last_openings.push(opening.clone());
        } else {
            ranges.push(closing.clone());
        }
        openings.push(opening);
        closings.push(closing);
    }
    let spelling = |mut spans: Vec<std::ops::Range<usize>>| {
        spans.sort_by_key(|span| span.start);
        spans.into_iter().fold(Fragment::empty(), |fragment, span| {
            doc.slice(span.start, span.end).map_or_else(
                |_| fragment.clone(),
                |slice| fragment.append(slice.content()),
            )
        })
    };
    let opening = spelling(openings);
    let closing = spelling(closings);
    let first_closing = spelling(first_closings);
    let last_opening = spelling(last_openings);
    let last = slice.content().child_count() - 1;
    let nodes = slice.content().iter().enumerate().map(|(index, node)| {
        let mut content = node.content().clone();
        if index > 0 && (index == last || node.content_size() > 0) {
            content = if index == last {
                last_opening.append(&content)
            } else {
                opening.append(&content)
            };
        }
        if index < last && (index == 0 || node.content_size() > 0) {
            content = content.append(if index == 0 { &first_closing } else { &closing });
        }
        node.copy(content)
    });
    let slice = Slice::new(
        Fragment::from_nodes(nodes),
        slice.open_start(),
        slice.open_end(),
    );
    ranges.sort_by_key(|range| range.start);
    let mut merged: Vec<std::ops::Range<usize>> = Vec::new();
    for range in ranges {
        if let Some(previous) = merged.last_mut()
            && range.start <= previous.end
        {
            previous.end = previous.end.max(range.end);
        } else {
            merged.push(range);
        }
    }
    (slice, merged)
}
