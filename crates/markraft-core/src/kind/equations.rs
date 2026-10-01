//! Document-wide equation numbering and references, derived without editing source.
//!
//! Numbering is deliberately per standalone display formula. AMS environments
//! with multiple rows receive a single block number; starred environments stay
//! unnumbered. Macro bodies are opaque and remain the renderer's responsibility.

use std::collections::BTreeMap;
use std::ops::Range;

use super::{DocTypes, math::formula_spans};
use crate::projection::Projection;

/// A semantic equation issue. Presentation layers choose how to localize it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EquationDiagnostic {
    /// An inline formula contains display-only metadata.
    StandaloneTag,
    /// A macro redefines a reserved metadata command.
    MetadataRedefined,
    /// Metadata appears inside a multi-row environment.
    PerRowMetadata,
    /// A metadata command or reference is incomplete.
    IncompleteMetadata,
    /// One formula declares multiple tags.
    MultipleTags,
    /// A tag has no content.
    EmptyTag,
    /// A tag contains a reference.
    TagReference,
    /// A label has no name.
    EmptyLabel,
    /// Multiple formulas share this displayed number.
    DuplicateNumber(String),
    /// Multiple formulas share this label.
    DuplicateLabel(String),
    /// This reference matches multiple labels.
    AmbiguousReference(String),
    /// This reference has no matching label.
    UnknownReference(String),
    /// This reference targets an unnumbered equation.
    UnnumberedReference(String),
    /// This environment is not supported.
    UnsupportedEnvironment(String),
}

impl std::fmt::Display for EquationDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StandaloneTag => f.write_str("Equation tags require a standalone display formula"),
            Self::MetadataRedefined => f.write_str("Redefining equation metadata commands is not supported"),
            Self::PerRowMetadata => f.write_str("Per-row equation metadata is not supported yet; place block metadata after the environment"),
            Self::IncompleteMetadata => f.write_str("Incomplete equation metadata or reference"),
            Self::MultipleTags => f.write_str("A formula can have only one equation tag"),
            Self::EmptyTag => f.write_str("Equation tags cannot be empty"),
            Self::TagReference => f.write_str("Equation tags cannot contain references"),
            Self::EmptyLabel => f.write_str("Equation labels cannot be empty"),
            Self::DuplicateNumber(value) => write!(f, "Duplicate equation number: {value}"),
            Self::DuplicateLabel(value) => write!(f, "Duplicate equation label: {value}"),
            Self::AmbiguousReference(value) => write!(f, "Ambiguous equation reference: {value}"),
            Self::UnknownReference(value) => write!(f, "Unknown equation reference: {value}"),
            Self::UnnumberedReference(value) => write!(f, "Equation has no number: {value}"),
            Self::UnsupportedEnvironment(value) => write!(f, "The {value} environment is not supported yet"),
        }
    }
}

/// Renderable information for one formula, in projected source coordinates.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Equation {
    /// Formula boundaries, including delimiters, in projected characters.
    pub source: Range<usize>,
    /// Absolute document position where the formula's TeX begins, just past its
    /// opening delimiter, when the projection maps it back to the tree.
    pub position: Option<usize>,
    /// TeX body with numbering metadata removed and references resolved.
    pub render_source: String,
    /// TeX for the separately laid-out tag, including its desired parentheses.
    pub tag: Option<String>,
    /// Absolute document position for a formula consisting of a single reference.
    pub target: Option<usize>,
    /// A semantic issue to show alongside the editable original source.
    pub diagnostic: Option<EquationDiagnostic>,
}

/// A deterministic index keyed by projected line and formula source start.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EquationIndex {
    by_source: BTreeMap<(usize, usize), Equation>,
    by_position: BTreeMap<usize, (usize, usize)>,
}

impl EquationIndex {
    /// Look up one formula by its projected location.
    pub fn get(&self, line_index: usize, source_start: usize) -> Option<&Equation> {
        self.by_source.get(&(line_index, source_start))
    }

    /// Look up one formula by the document position where its TeX begins
    /// ([`Equation::position`]), for consumers that walk the tree rather than
    /// the projection.
    pub fn at_position(&self, position: usize) -> Option<&Equation> {
        self.by_position
            .get(&position)
            .and_then(|key| self.by_source.get(key))
    }

    /// Resolve labels in a separate pass so forward references and offscreen
    /// formulas behave exactly like backward and visible references.
    pub fn build(projection: &Projection, types: &DocTypes, auto_number: bool) -> Self {
        let mut pending = Vec::new();
        let mut labels: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        let mut next = 1;
        for (line_index, line) in projection.lines().iter().enumerate() {
            let source = projection.line_text(line_index).unwrap_or_default();
            for span in formula_spans(line, source, types) {
                let mut parsed = Parsed::scan(&span.tex);
                let standalone = span.is_standalone(source);
                if !standalone && parsed.tag.is_some() {
                    parsed.issue(EquationDiagnostic::StandaloneTag);
                }
                let body = parsed.with_references(&span.tex, |_, _| String::new());
                let nonempty = !body.trim().is_empty();
                let tag = if standalone && nonempty && parsed.diagnostic.is_none() {
                    parsed.tag.clone().or_else(|| {
                        if auto_number && !parsed.suppressed {
                            let tag = Tag {
                                body: next.to_string(),
                                parentheses: true,
                            };
                            next += 1;
                            Some(tag)
                        } else {
                            None
                        }
                    })
                } else {
                    None
                };
                let ordinal = pending.len();
                for label in &parsed.labels {
                    labels.entry(label.clone()).or_default().push(ordinal);
                }
                pending.push(Pending {
                    key: (line_index, span.source.start),
                    position: line.offset_to_pos(span.content.start),
                    source: span.source,
                    tex: span.tex,
                    parsed,
                    tag,
                });
            }
        }
        let mut shown_tags: BTreeMap<String, usize> = BTreeMap::new();
        for tag in pending.iter().filter_map(|item| item.tag.as_ref()) {
            *shown_tags.entry(tag.render()).or_default() += 1;
        }
        let mut result = Self::default();
        for item in &pending {
            let mut diagnostic = item.parsed.diagnostic.clone();
            if let Some(tag) = item.tag.as_ref().map(Tag::render)
                && shown_tags[&tag] > 1
            {
                // A manual tag may repeat an automatic number; say so rather
                // than show two equations under one number.
                issue(&mut diagnostic, EquationDiagnostic::DuplicateNumber(tag));
            }
            for label in &item.parsed.labels {
                if labels[label].len() > 1 {
                    issue(
                        &mut diagnostic,
                        EquationDiagnostic::DuplicateLabel(label.clone()),
                    );
                }
            }
            let mut resolved_targets = Vec::new();
            let render_source = item
                .parsed
                .with_references(&item.tex, |label, parentheses| {
                    let target = match labels.get(label).map(Vec::as_slice) {
                        Some([index]) => &pending[*index],
                        Some(_) => {
                            issue(
                                &mut diagnostic,
                                EquationDiagnostic::AmbiguousReference(label.to_owned()),
                            );
                            return "??".into();
                        }
                        None => {
                            issue(
                                &mut diagnostic,
                                EquationDiagnostic::UnknownReference(label.to_owned()),
                            );
                            return "??".into();
                        }
                    };
                    let Some(tag) = &target.tag else {
                        issue(
                            &mut diagnostic,
                            EquationDiagnostic::UnnumberedReference(label.to_owned()),
                        );
                        return "??".into();
                    };
                    resolved_targets.push(target.position);
                    if parentheses {
                        format!("({})", tag.body)
                    } else {
                        tag.body.clone()
                    }
                });
            let target = if item.parsed.references() == 1
                && item
                    .parsed
                    .with_references(&item.tex, |_, _| String::new())
                    .trim()
                    .is_empty()
            {
                resolved_targets.first().copied().flatten()
            } else {
                None
            };
            if let Some(position) = item.position {
                result.by_position.insert(position, item.key);
            }
            result.by_source.insert(
                item.key,
                Equation {
                    source: item.source.clone(),
                    position: item.position,
                    render_source,
                    tag: item.tag.as_ref().map(Tag::render),
                    target,
                    diagnostic,
                },
            );
        }
        result
    }
}

struct Pending {
    key: (usize, usize),
    position: Option<usize>,
    source: Range<usize>,
    tex: String,
    parsed: Parsed,
    tag: Option<Tag>,
}

#[derive(Clone)]
struct Tag {
    body: String,
    parentheses: bool,
}
impl Tag {
    fn render(&self) -> String {
        if self.parentheses {
            format!("({})", self.body)
        } else {
            self.body.clone()
        }
    }
}

#[derive(Default)]
struct Parsed {
    edits: Vec<(Range<usize>, Edit)>,
    labels: Vec<String>,
    tag: Option<Tag>,
    suppressed: bool,
    diagnostic: Option<EquationDiagnostic>,
}
enum Edit {
    Remove,
    Literal(String),
    Reference(Reference),
}
struct Reference {
    label: String,
    parentheses: bool,
}

impl Parsed {
    fn issue(&mut self, message: EquationDiagnostic) {
        issue(&mut self.diagnostic, message);
    }

    fn references(&self) -> usize {
        self.edits
            .iter()
            .filter(|(_, value)| matches!(value, Edit::Reference(_)))
            .count()
    }

    fn with_references(
        &self,
        source: &str,
        mut reference: impl FnMut(&str, bool) -> String,
    ) -> String {
        let mut result = String::new();
        let mut cursor = 0;
        for (range, value) in &self.edits {
            result.push_str(&source[cursor..range.start]);
            if let Edit::Reference(value) = value {
                let replacement = reference(&value.label, value.parentheses);
                if !replacement.is_empty() {
                    // Group substitutions so a tag ending in a control word cannot
                    // merge with following source, or change a command argument.
                    result.push('{');
                    result.push_str(&replacement);
                    result.push('}');
                }
            } else if let Edit::Literal(text) = value {
                result.push_str(text);
            } else if !source[range.clone()].starts_with('%') {
                // Removing metadata must not concatenate two TeX tokens.
                result.push(' ');
            }
            cursor = range.end;
        }
        result.push_str(&source[cursor..]);
        result
    }

    fn scan(source: &str) -> Self {
        let mut result = Self::default();
        let bytes = source.as_bytes();
        let mut cursor = 0;
        let mut depth = 0usize;
        let mut numbered_environment_depth = 0usize;
        while cursor < bytes.len() {
            match bytes[cursor] {
                b'%' => {
                    let end = source[cursor..]
                        .find('\n')
                        .map_or(bytes.len(), |n| cursor + n);
                    result.edits.push((cursor..end, Edit::Remove));
                    cursor = end;
                }
                b'{' => {
                    depth += 1;
                    cursor += 1;
                }
                b'}' => {
                    depth = depth.saturating_sub(1);
                    cursor += 1;
                }
                b'\\' => {
                    let start = cursor;
                    cursor += 1;
                    let name_start = cursor;
                    while cursor < bytes.len() && bytes[cursor].is_ascii_alphabetic() {
                        cursor += 1;
                    }
                    if cursor == name_start {
                        // TeX control symbols consume exactly one character, including
                        // escaped braces, percent signs and the second slash of `\\`.
                        if cursor < bytes.len() {
                            cursor += source[cursor..].chars().next().unwrap().len_utf8();
                        }
                        continue;
                    }
                    let name = &source[name_start..cursor];
                    if matches!(
                        name,
                        "def"
                            | "gdef"
                            | "edef"
                            | "xdef"
                            | "let"
                            | "newcommand"
                            | "renewcommand"
                            | "providecommand"
                    ) {
                        let (end, redefines_metadata) = macro_end(source, cursor, name);
                        if redefines_metadata {
                            result.issue(EquationDiagnostic::MetadataRedefined);
                        }
                        cursor = end;
                        continue;
                    }
                    if matches!(
                        name,
                        "text"
                            | "textrm"
                            | "textnormal"
                            | "textbf"
                            | "textit"
                            | "texttt"
                            | "textsf"
                            | "mbox"
                            | "hbox"
                            | "operatorname"
                            | "verb"
                    ) {
                        if bytes.get(cursor) == Some(&b'*') {
                            cursor += 1;
                        }
                        if name == "verb" {
                            // The delimiter is any character, so step by characters.
                            if let Some(delimiter) = source[cursor..].chars().next() {
                                cursor += delimiter.len_utf8();
                                if let Some(end) = source[cursor..].find(delimiter) {
                                    cursor += end + delimiter.len_utf8();
                                }
                            }
                        } else if let Some((_, end)) = group(source, skip_space(source, cursor)) {
                            cursor = end;
                        }
                        continue;
                    }
                    if matches!(name, "begin" | "end") {
                        if let Some((body, end)) = group(source, skip_space(source, cursor)) {
                            let environment = body.trim_end_matches('*');
                            if name == "begin"
                                && matches!(environment, "multline" | "eqnarray" | "flalign")
                            {
                                // The renderer cannot draw these, so they must
                                // not take a number from the formulas after them.
                                result.issue(EquationDiagnostic::UnsupportedEnvironment(
                                    environment.to_owned(),
                                ));
                            }
                            if matches!(environment, "align" | "alignat" | "gather") {
                                if name == "begin" {
                                    numbered_environment_depth += 1;
                                } else {
                                    numbered_environment_depth =
                                        numbered_environment_depth.saturating_sub(1);
                                }
                                if body.ends_with('*') {
                                    result.suppressed = true;
                                } else {
                                    result.edits.push((
                                        start..end,
                                        Edit::Literal(format!("\\{name}{{{body}*}}")),
                                    ));
                                }
                            }
                            if matches!(body, "equation" | "equation*" | "displaymath") {
                                // The document owns numbering; prevent the renderer
                                // from restarting its private counter for every block.
                                result.edits.push((start..end, Edit::Remove));
                                if body == "equation*" {
                                    result.suppressed = true;
                                }
                            }
                            cursor = end;
                        }
                        continue;
                    }
                    if depth == 0
                        && numbered_environment_depth > 0
                        && matches!(name, "tag" | "label" | "notag" | "nonumber")
                    {
                        result.issue(EquationDiagnostic::PerRowMetadata);
                    }
                    if depth == 0 && matches!(name, "notag" | "nonumber") {
                        result.suppressed = true;
                        result.edits.push((start..cursor, Edit::Remove));
                        continue;
                    }
                    if !(matches!(name, "ref" | "eqref")
                        || depth == 0 && matches!(name, "tag" | "label"))
                    {
                        continue;
                    }
                    let starred = name == "tag" && bytes.get(cursor) == Some(&b'*');
                    if starred {
                        cursor += 1;
                    }
                    let Some((body, end)) = group(source, skip_space(source, cursor)) else {
                        result.issue(EquationDiagnostic::IncompleteMetadata);
                        continue;
                    };
                    cursor = end;
                    match name {
                        "tag" => {
                            if result.tag.is_some() {
                                result.issue(EquationDiagnostic::MultipleTags);
                            }
                            if body.trim().is_empty() {
                                result.issue(EquationDiagnostic::EmptyTag);
                            }
                            if contains_reference(body) {
                                result.issue(EquationDiagnostic::TagReference);
                            }
                            result.tag = Some(Tag {
                                body: body.to_owned(),
                                parentheses: !starred,
                            });
                            result.edits.push((start..end, Edit::Remove));
                        }
                        "label" => {
                            if body.trim().is_empty() {
                                result.issue(EquationDiagnostic::EmptyLabel);
                            } else {
                                result.labels.push(body.trim().to_owned());
                            }
                            result.edits.push((start..end, Edit::Remove));
                        }
                        _ => result.edits.push((
                            start..end,
                            Edit::Reference(Reference {
                                label: body.trim().to_owned(),
                                parentheses: name == "eqref",
                            }),
                        )),
                    }
                }
                _ => cursor += source[cursor..].chars().next().unwrap().len_utf8(),
            }
        }
        result
    }
}

fn issue(target: &mut Option<EquationDiagnostic>, message: EquationDiagnostic) {
    if target.is_none() {
        *target = Some(message);
    }
}

/// Tags cannot depend on references: resolving them would introduce cycles.
fn contains_reference(source: &str) -> bool {
    let mut cursor = 0;
    while cursor < source.len() {
        match source.as_bytes()[cursor] {
            b'%' => {
                cursor = source[cursor..]
                    .find('\n')
                    .map_or(source.len(), |n| cursor + n + 1);
            }
            b'\\' => {
                cursor += 1;
                let start = cursor;
                while source
                    .as_bytes()
                    .get(cursor)
                    .is_some_and(u8::is_ascii_alphabetic)
                {
                    cursor += 1;
                }
                let name = &source[start..cursor];
                if matches!(name, "ref" | "eqref") {
                    return true;
                }
                if name.starts_with("text") || matches!(name, "mbox" | "hbox" | "operatorname") {
                    if let Some((_, end)) = group(source, skip_space(source, cursor)) {
                        cursor = end;
                    }
                } else if name.is_empty() && cursor < source.len() {
                    cursor += source[cursor..].chars().next().unwrap().len_utf8();
                }
            }
            _ => cursor += source[cursor..].chars().next().unwrap().len_utf8(),
        }
    }
    false
}

/// Skip a macro definition without assigning semantics to its replacement body.
fn macro_end(source: &str, from: usize, command: &str) -> (usize, bool) {
    let mut cursor = skip_space(source, from);
    if source.as_bytes().get(cursor) == Some(&b'*') {
        cursor = skip_space(source, cursor + 1);
    }
    let (name, end) = if let Some((name, end)) = group(source, cursor) {
        (name, end)
    } else {
        let start = cursor;
        if source.as_bytes().get(cursor) == Some(&b'\\') {
            cursor += 1;
        }
        while source
            .as_bytes()
            .get(cursor)
            .is_some_and(u8::is_ascii_alphabetic)
        {
            cursor += 1;
        }
        (&source[start..cursor], cursor)
    };
    let redefines = matches!(
        name.trim().trim_start_matches('\\'),
        "tag" | "label" | "ref" | "eqref" | "notag" | "nonumber"
    );
    cursor = skip_space(source, end);
    if command == "let" {
        if source.as_bytes().get(cursor) == Some(&b'=') {
            cursor = skip_space(source, cursor + 1);
        }
        if source.as_bytes().get(cursor) == Some(&b'\\') {
            cursor += 1;
            while source
                .as_bytes()
                .get(cursor)
                .is_some_and(u8::is_ascii_alphabetic)
            {
                cursor += 1;
            }
        } else if cursor < source.len() {
            cursor += source[cursor..].chars().next().unwrap().len_utf8();
        }
        return (cursor, redefines);
    }
    // Optional argument count/defaults and primitive parameter tokens precede
    // the replacement group. They cannot introduce document labels either.
    while cursor < source.len() {
        if source.as_bytes()[cursor] == b'{' {
            return (
                group(source, cursor).map_or(source.len(), |(_, end)| end),
                redefines,
            );
        }
        if source.as_bytes()[cursor] == b'[' {
            cursor += 1;
            while cursor < source.len() && source.as_bytes()[cursor] != b']' {
                if let Some((_, end)) = group(source, cursor) {
                    cursor = end;
                } else {
                    cursor += source[cursor..].chars().next().unwrap().len_utf8();
                }
            }
            if cursor < source.len() {
                cursor += 1;
            }
        } else {
            cursor += source[cursor..].chars().next().unwrap().len_utf8();
        }
    }
    (cursor, redefines)
}

fn skip_space(source: &str, from: usize) -> usize {
    from + source[from..].len() - source[from..].trim_start().len()
}

/// A balanced TeX group; escaped braces and braces inside comments are literal.
fn group(source: &str, start: usize) -> Option<(&str, usize)> {
    let bytes = source.as_bytes();
    if bytes.get(start) != Some(&b'{') {
        return None;
    }
    let mut depth = 1;
    let mut cursor = start + 1;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\\' => {
                cursor += 1;
                if cursor < bytes.len() {
                    cursor += source[cursor..].chars().next()?.len_utf8();
                }
                continue;
            }
            b'%' => {
                cursor = source[cursor..]
                    .find('\n')
                    .map_or(bytes.len(), |n| cursor + n);
                continue;
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((&source[start + 1..cursor], cursor + 1));
                }
            }
            _ => {}
        }
        cursor += source[cursor..].chars().next()?.len_utf8();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attr::{AttrKind, AttrSpec, AttrValue};
    use crate::schema::{MarkTypeSpec, NodeTypeSpec, Schema, SchemaSpec};
    use crate::{Mark, MarkSet, attrs};

    fn projection(formulas: &[(&str, bool)]) -> (Projection, DocTypes) {
        let schema = Schema::new(
            SchemaSpec::new()
                .node(NodeTypeSpec::new("doc", "paragraph+"))
                .node(NodeTypeSpec::new("paragraph", "text*"))
                .node(NodeTypeSpec::text("text"))
                .mark(MarkTypeSpec::new("math").attr(AttrSpec::new(
                    "display",
                    AttrKind::Bool,
                    AttrValue::Bool(false),
                )))
                .mark(
                    MarkTypeSpec::new("syntax")
                        .attr(AttrSpec::new("span", AttrKind::Int, AttrValue::Int(0)))
                        .attr(AttrSpec::new(
                            "display",
                            AttrKind::Str,
                            AttrValue::Str("".into()),
                        )),
                ),
        )
        .unwrap();
        let types = DocTypes {
            math: schema.mark_id("math"),
            syntax: schema.mark_id("syntax"),
            ..Default::default()
        };
        let blocks = formulas.iter().map(|(tex, display)| {
            let math = Mark::with_attrs(types.math.unwrap(), attrs! { "display" => *display });
            let body_marks = MarkSet::from_marks(&schema, [math.clone()]);
            let fences = MarkSet::from_marks(
                &schema,
                [
                    math,
                    Mark::with_attrs(
                        types.syntax.unwrap(),
                        attrs! { "span" => 1i64, "display" => "" },
                    ),
                ],
            );
            let fence = if *display { "$$" } else { "$" };
            let mut content = vec![schema.text_marked(fence, fences.clone())];
            if !tex.is_empty() {
                content.push(schema.text_marked(tex, body_marks));
            }
            content.push(schema.text_marked(fence, fences));
            schema.node("paragraph", content).unwrap()
        });
        let doc = schema.node("doc", blocks).unwrap();
        (Projection::of(&doc, &schema), types)
    }

    fn index(formulas: &[(&str, bool)], auto: bool) -> EquationIndex {
        let (projection, types) = projection(formulas);
        EquationIndex::build(&projection, &types, auto)
    }

    #[test]
    fn numbering_is_document_order_and_manual_tags_do_not_consume_numbers() {
        let equations = index(
            &[
                ("a", true),
                (r"b\tag{B}", true),
                ("c", false),
                (r"d\notag", true),
                ("e", true),
            ],
            true,
        );
        let tags: Vec<_> = (0..5)
            .map(|i| equations.get(i, 0).unwrap().tag.as_deref())
            .collect();
        assert_eq!(tags, [Some("(1)"), Some("(B)"), None, None, Some("(2)")]);
        assert_eq!(index(&[("a", true)], false).get(0, 0).unwrap().tag, None);
    }

    #[test]
    fn forward_references_resolve_and_pure_references_link_to_source() {
        let (projection, types) = projection(&[
            (r"\eqref{energy}", false),
            (r"E=mc^2\label{energy}", true),
            (r"x+\ref{energy}", false),
        ]);
        let equations = EquationIndex::build(&projection, &types, true);
        let reference = equations.get(0, 0).unwrap();
        assert_eq!(reference.render_source, "{(1)}");
        assert_eq!(
            reference.target,
            projection.line(1).unwrap().offset_to_pos(2)
        );
        assert_eq!(equations.get(2, 0).unwrap().render_source, "x+{1}");
        assert_eq!(equations.get(2, 0).unwrap().target, None);
        assert_eq!(
            projection.line_text(1).unwrap(),
            r"$$E=mc^2\label{energy}$$"
        );
    }

    #[test]
    fn starred_tags_and_reference_parentheses_are_independent() {
        let equations = index(
            &[
                (r"a\tag*{A_{1}}\label{a}", true),
                (r"\ref{a}", false),
                (r"\eqref{a}", false),
            ],
            false,
        );
        assert_eq!(equations.get(0, 0).unwrap().tag.as_deref(), Some("A_{1}"));
        assert_eq!(equations.get(1, 0).unwrap().render_source, "{A_{1}}");
        assert_eq!(equations.get(2, 0).unwrap().render_source, "{(A_{1})}");
    }

    #[test]
    fn ambiguous_missing_and_unnumbered_labels_are_diagnosed() {
        let equations = index(
            &[
                (r"a\label{a}", true),
                (r"b\label{a}", true),
                (r"\ref{a}", false),
                (r"\ref{missing}", false),
            ],
            true,
        );
        assert!(
            equations
                .get(0, 0)
                .unwrap()
                .diagnostic
                .as_ref()
                .unwrap()
                .to_string()
                .contains("Duplicate")
        );
        assert!(
            equations
                .get(1, 0)
                .unwrap()
                .diagnostic
                .as_ref()
                .unwrap()
                .to_string()
                .contains("Duplicate")
        );
        assert!(
            equations
                .get(2, 0)
                .unwrap()
                .diagnostic
                .as_ref()
                .unwrap()
                .to_string()
                .contains("Ambiguous")
        );
        assert_eq!(equations.get(2, 0).unwrap().target, None);
        assert!(
            equations
                .get(3, 0)
                .unwrap()
                .diagnostic
                .as_ref()
                .unwrap()
                .to_string()
                .contains("Unknown")
        );
        let equations = index(&[(r"a\label{a}", true), (r"\ref{a}", false)], false);
        assert!(
            equations
                .get(1, 0)
                .unwrap()
                .diagnostic
                .as_ref()
                .unwrap()
                .to_string()
                .contains("no number")
        );
    }

    #[test]
    fn verbatim_with_a_multibyte_delimiter_is_skipped_by_characters() {
        for source in [
            r"x \verbéaéb\label{a}",
            r"x \verb中",
            r"\verb*αβα\label{a}",
            r"\verbé",
        ] {
            let parsed = Parsed::scan(source);
            assert_eq!(parsed.diagnostic, None, "{source:?}");
        }
        assert_eq!(Parsed::scan(r"\verbé\label{b}é\label{a}").labels, ["a"]);
    }

    #[test]
    fn environments_the_renderer_lacks_are_diagnosed_instead_of_numbered() {
        for environment in ["multline", "eqnarray", "flalign", "multline*"] {
            let equations = index(
                &[
                    (
                        &format!(r"\begin{{{environment}}}a\\b\end{{{environment}}}"),
                        true,
                    ),
                    ("c", true),
                ],
                true,
            );
            let first = equations.get(0, 0).unwrap();
            assert_eq!(first.tag, None, "{environment}");
            assert!(
                first
                    .diagnostic
                    .as_ref()
                    .unwrap()
                    .to_string()
                    .contains(environment.trim_end_matches('*')),
                "{environment}"
            );
            assert_eq!(equations.get(1, 0).unwrap().tag.as_deref(), Some("(1)"));
        }
    }

    #[test]
    fn a_manual_tag_that_repeats_an_automatic_number_is_diagnosed() {
        let equations = index(&[(r"a\tag{1}", true), ("b", true), ("c", true)], true);
        let tags: Vec<_> = (0..3)
            .map(|i| equations.get(i, 0).unwrap().tag.as_deref())
            .collect();
        assert_eq!(tags, [Some("(1)"), Some("(1)"), Some("(2)")]);
        for line in 0..2 {
            assert!(
                equations
                    .get(line, 0)
                    .unwrap()
                    .diagnostic
                    .as_ref()
                    .unwrap()
                    .to_string()
                    .contains("Duplicate equation number: (1)")
            );
        }
        assert_eq!(equations.get(2, 0).unwrap().diagnostic, None);
        // Starred tags print without parentheses and never collide with them.
        let equations = index(&[(r"a\tag*{1}", true), ("b", true)], true);
        assert_eq!(equations.get(0, 0).unwrap().diagnostic, None);
    }

    #[test]
    fn escapes_comments_and_text_groups_do_not_define_labels() {
        let parsed =
            Parsed::scan("x % \\tag{bad}\n\\text{\\label{bad}}+\\\\label{also-bad}+y\\label{good}");
        assert_eq!(parsed.labels, ["good"]);
        assert!(parsed.tag.is_none());
        assert_eq!(
            parsed.with_references(
                "x % \\tag{bad}\n\\text{\\label{bad}}+\\\\label{also-bad}+y\\label{good}",
                |_, _| unreachable!()
            ),
            "x \n\\text{\\label{bad}}+\\\\label{also-bad}+y "
        );
        let parsed = Parsed::scan(r"x\tag{\{a\}_{1}}\label{α}");
        assert_eq!(parsed.tag.unwrap().body, r"\{a\}_{1}");
        assert_eq!(parsed.labels, ["α"]);
    }

    #[test]
    fn references_in_math_groups_resolve_but_macro_definitions_remain_opaque() {
        let equations = index(
            &[(r"a\tag{A}\label{a}", true), (r"\frac{\ref{a}}{2}", false)],
            false,
        );
        assert_eq!(equations.get(1, 0).unwrap().render_source, r"\frac{{A}}{2}");
        let parsed = Parsed::scan(r"\newcommand{\foo}{\label{fake}}\foo");
        assert!(parsed.labels.is_empty());
        assert!(parsed.diagnostic.is_none());
    }

    #[test]
    fn invalid_tags_fail_explicitly() {
        for source in [r"x\tag{A}\tag{B}", r"x\tag{", r"x\tag{}"] {
            let equations = index(&[(source, true)], true);
            let formula = equations.get(0, 0).unwrap();
            assert!(formula.diagnostic.is_some(), "{source}");
            assert!(formula.tag.is_none());
        }
        assert!(
            index(&[(r"x\tag{A}", false)], true)
                .get(0, 0)
                .unwrap()
                .diagnostic
                .is_some()
        );
    }

    #[test]
    fn rebuilding_after_insert_delete_and_move_reassigns_numbers() {
        let before = index(
            &[
                (r"a\label{a}", true),
                (r"b\label{b}", true),
                (r"\eqref{b}", false),
            ],
            true,
        );
        let inserted = index(
            &[
                ("new", true),
                (r"a\label{a}", true),
                (r"b\label{b}", true),
                (r"\eqref{b}", false),
            ],
            true,
        );
        assert_eq!(before.get(2, 0).unwrap().render_source, "{(2)}");
        assert_eq!(inserted.get(3, 0).unwrap().render_source, "{(3)}");
        let moved = index(
            &[
                (r"b\label{b}", true),
                (r"a\label{a}", true),
                (r"\eqref{b}", false),
            ],
            true,
        );
        assert_eq!(moved.get(2, 0).unwrap().render_source, "{(1)}");
        let deleted = index(&[(r"b\label{b}", true), (r"\eqref{b}", false)], true);
        assert_eq!(deleted.get(1, 0).unwrap().render_source, "{(1)}");
    }
    #[test]
    fn substitutions_preserve_tex_token_and_argument_boundaries() {
        let equations = index(
            &[
                (r"a\tag{12}\label{a}", true),
                (r"x^\ref{a}", false),
                (r"\alpha\label{b}b", true),
            ],
            true,
        );
        assert_eq!(equations.get(1, 0).unwrap().render_source, r"x^{12}");
        assert_eq!(equations.get(2, 0).unwrap().render_source, r"\alpha b");
        assert!(
            index(&[(r"x\tag{\ref{a}}", true)], false)
                .get(0, 0)
                .unwrap()
                .diagnostic
                .as_ref()
                .unwrap()
                .to_string()
                .contains("cannot contain references")
        );
    }

    #[test]
    fn macros_are_preserved_without_indexing_their_bodies() {
        let source = r"\newcommand{\foo}[1]{\text{#1}\label{fake}}\foo{x}\label{real}";
        let parsed = Parsed::scan(source);
        assert_eq!(parsed.labels, ["real"]);
        assert!(parsed.diagnostic.is_none());
        assert!(
            parsed
                .with_references(source, |_, _| unreachable!())
                .starts_with(r"\newcommand{\foo}[1]{\text{#1}\label{fake}}\foo{x}")
        );
        assert!(
            Parsed::scan(r"\renewcommand{\label}[1]{x}")
                .diagnostic
                .is_some()
        );
        assert!(Parsed::scan(r"\def\ref#1{x}").diagnostic.is_some());
    }

    #[test]
    fn ams_wrappers_do_not_restart_renderer_counters_or_reject_starred_math() {
        let source = r"\begin{align}a&=b\\c&=d\end{align}";
        let numbered = index(&[(source, true)], true);
        let formula = numbered.get(0, 0).unwrap();
        assert_eq!(formula.tag.as_deref(), Some("(1)"));
        assert_eq!(
            formula.render_source,
            r"\begin{align*}a&=b\\c&=d\end{align*}"
        );
        assert!(formula.diagnostic.is_none());
        let unnumbered = index(&[(source, true)], false);
        assert!(unnumbered.get(0, 0).unwrap().tag.is_none());
        let starred = index(&[(r"\begin{align*}a&=b\end{align*}", true)], true);
        assert!(starred.get(0, 0).unwrap().tag.is_none());
        assert!(starred.get(0, 0).unwrap().diagnostic.is_none());
        let single = index(&[(r"\begin{equation}x=1\end{equation}", true)], true);
        assert_eq!(single.get(0, 0).unwrap().render_source.trim(), "x=1");
    }
    #[test]
    fn per_row_metadata_is_explicitly_distinguished_from_block_metadata() {
        let row_label = index(&[(r"\begin{align}a&=b\label{a}\end{align}", true)], true);
        assert!(
            row_label
                .get(0, 0)
                .unwrap()
                .diagnostic
                .as_ref()
                .unwrap()
                .to_string()
                .contains("Per-row")
        );
        let block_label = index(
            &[
                (r"\begin{align}a&=b\end{align}\label{a}", true),
                (r"\ref{a}", false),
            ],
            true,
        );
        assert!(block_label.get(0, 0).unwrap().diagnostic.is_none());
        assert_eq!(block_label.get(1, 0).unwrap().render_source, "{1}");
        let parsed = Parsed::scan(r"\operatorname*{\ref{literal}}\label{real}");
        assert_eq!(parsed.references(), 0);
        assert_eq!(parsed.labels, ["real"]);
    }
}
